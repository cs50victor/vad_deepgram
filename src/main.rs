// derived from https://github.com/deepgram-devs/deepgram-rust-sdk/pull/22
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use deepgram::DeepgramError;
use ezsockets::{
    client::ClientCloseMode, Client, ClientConfig, CloseFrame, MessageStatus, RawMessage,
    SocketConfig, WSError,
};
use log::{error, info};
use rand::Rng;
use serde_json::{json, Value};

static PATH_TO_FILE: &str = "mkbhd_video.mp3";

#[derive(Clone)]
pub struct STT {
    ws_client: Arc<Client<WSClient>>,
}

struct WSClient {
    llm_channel_tx: crossbeam_channel::Sender<String>,
}

#[async_trait]
impl ezsockets::ClientExt for WSClient {
    type Call = ();

    async fn on_text(&mut self, text: String) -> Result<(), ezsockets::Error> {
        let data: Value = serde_json::from_str(&text)?;
        let transcript = data["channel"]["alternatives"][0]["transcript"].clone();

        if transcript != Value::Null {
            if let Err(e) = self.llm_channel_tx.send(transcript.to_string()) {
                error!("Error sending to LLM: {}", e);
            };
        }

        Ok(())
    }

    async fn on_binary(&mut self, bytes: Vec<u8>) -> Result<(), ezsockets::Error> {
        info!("received bytes: {bytes:?}");
        Ok(())
    }

    async fn on_call(&mut self, call: Self::Call) -> Result<(), ezsockets::Error> {
        info!("Deepgram ON CALL: {call:?}");
        let () = call;
        Ok(())
    }
    async fn on_connect(&mut self) -> Result<(), ezsockets::Error> {
        info!("Deepgram CONNECTED");
        Ok(())
    }

    async fn on_connect_fail(&mut self, e: WSError) -> Result<ClientCloseMode, ezsockets::Error> {
        error!("Deepgram CONNECTION FAILED | {e}");
        Ok(ClientCloseMode::Reconnect)
    }

    async fn on_close(
        &mut self,
        frame: Option<CloseFrame>,
    ) -> Result<ClientCloseMode, ezsockets::Error> {
        error!("Deepgram CONNECTION CLOSED | {frame:?}");
        Ok(ClientCloseMode::Reconnect)
    }

    async fn on_disconnect(&mut self) -> Result<ClientCloseMode, ezsockets::Error> {
        error!("Deepgram disconnect");
        Ok(ClientCloseMode::Reconnect)
    }
}

impl STT {
    pub async fn new(llm_channel_tx: crossbeam_channel::Sender<String>) -> anyhow::Result<Self> {
        let deepgram_api_key = std::env::var("DEEPGRAM_API_KEY").unwrap();

        let config = ClientConfig::new("wss://api.deepgram.com/v1/listen")
            .socket_config(SocketConfig {
                heartbeat: Duration::from_secs(11),
                timeout: Duration::from_secs(30 * 60), // 30 minutes
                heartbeat_ping_msg_fn: Arc::new(|_t: Duration| {
                    // really important
                    RawMessage::Text(
                        json!({
                            "type": "KeepAlive",
                        })
                        .to_string(),
                    )
                }),
            })
            .header("Authorization", &format!("Token {}", deepgram_api_key))
            .query_parameter("model", "nova-2-conversationalai")
            .query_parameter("smart_format", "true")
            .query_parameter("version", "latest")
            .query_parameter("filler_words", "true");

        let (ws_client, _) =
            ezsockets::connect(|_client| WSClient { llm_channel_tx }, config).await;

        Ok(Self {
            ws_client: Arc::new(ws_client),
        })
    }
    pub fn send(&self, bytes: impl Into<Vec<u8>>) -> anyhow::Result<MessageStatus> {
        let signal = self.ws_client.binary(bytes)?;
        Ok(signal.status())
    }
}

#[tokio::main]
async fn main() -> Result<(), DeepgramError> {
    pretty_env_logger::formatted_builder()
        .filter_module("deepgram_ws", log::LevelFilter::Info)
        .init();

    let (llm_tx, llm_rx) = crossbeam_channel::unbounded::<String>();

    let stt = STT::new(llm_tx).await.unwrap();
    let stt = stt.clone();

    let send_task = tokio::spawn(send_to_deepgram(stt));

    let deepgram_task = tokio::spawn(async move {
        while let Ok(msg) = llm_rx.recv() {
            info!("{msg}");
        }
    });

    send_task.await.unwrap()?;
    deepgram_task.await.unwrap();
    info!("Done!");
    Ok(())
}

async fn send_to_deepgram(stt: STT) -> Result<(), DeepgramError> {
    // simulate a user not speaking for the first couple of seconds
    simulate_long_pause(15).await;

    let audio_data = tokio::fs::read(PATH_TO_FILE).await.unwrap();

    static SLICE_SIZE: usize = 500;

    // Simulate an audio stream by sending the contents of a file in chunks
    let mut slice_begin = 0;
    while slice_begin < audio_data.len() {
        let slice_end = std::cmp::min(slice_begin + SLICE_SIZE, audio_data.len());

        let slice = &audio_data[slice_begin..slice_end];

        if let Err(e) = stt.send(slice) {
            error!("Error sending to Deepgram: {}", e);
        };

        slice_begin += SLICE_SIZE;

        // Randomly simulate a user not speaking for a few seconds
        let pause = rand::thread_rng().gen_range(0..10000);
        if pause%2==0 && pause < 14 {
            simulate_long_pause(pause).await;
        }
    }

    // Tell Deepgram that we've finished sending audio data by sending a zero-byte message
    if let Err(e) = stt.send([]) {
        error!("Error sending to Deepgram: {}", e);
    };

    Ok(())
}

async fn simulate_long_pause(pause: u64) {
    info!("Simulating a pause of {} seconds", pause);
    tokio::time::sleep(Duration::from_secs(pause)).await;
}