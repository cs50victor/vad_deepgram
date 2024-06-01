// derived from https://github.com/deepgram-devs/deepgram-rust-sdk/pull/22
#![feature(array_chunks)]
use std::{sync::Arc, time::{Duration, Instant}};

use async_trait::async_trait;
use bytes::Buf as _;
use deepgram::DeepgramError;
use ezsockets::{
    client::ClientCloseMode, Client, ClientConfig, CloseFrame, MessageStatus, RawMessage,
    SocketConfig, WSError,
};
use futures::{stream, StreamExt};
use samplerate::{ConverterType, Samplerate};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use serde_json::{json, Value};
use tracing_subscriber::{fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt};
use voice_activity_detector::VoiceActivityDetector;

static PATH_TO_FILE: &str = "mkbhd_video.mp3";

#[derive(Clone)]
pub struct STT {
    ws_client: Client<WSClient>,
}

struct WSClient {
    buffer: Vec<String>
}

#[async_trait]
impl ezsockets::ClientExt for WSClient {
    type Call = ();

    async fn on_text(&mut self, text: String) -> Result<(), ezsockets::Error> {
        let data: Value = serde_json::from_str(&text)?;
        let transcript = data["channel"]["alternatives"][0]["transcript"].clone();

        let transcript_is_final = data["is_final"] == Value::Bool(true);

        if let Value::String(transcript) = transcript {
            match self.buffer.len(){
                0 => self.buffer.push(transcript),
                n => self.buffer[n-1] = transcript
            };
            if transcript_is_final {
                self.buffer.push("".into());
            }
        };
        info!("current transcript -> {:?}", self.buffer);
        // if user_is_not_talking {
        //     let x = self.buffer.join(" ");
        //     self.buffer.clear();
        // }

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
    pub async fn new() -> anyhow::Result<Self> {
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
            .header("Authorization", &format!("Token {deepgram_api_key}"))
            .query_parameter("model", "nova-2-phonecall")
            .query_parameter("interim_results", "true")
            .query_parameter("smart_format", "true")
            .query_parameter("endpointing", &550.to_string())
            .query_parameter("utterance_end_ms", &1000.to_string())
            .query_parameter("filler_words", "true");

        let (ws_client, _) =
            ezsockets::connect(|_client| WSClient { 
                buffer: Vec::new()
             }, config).await;

        Ok(Self {
            ws_client,
        })
    }
    pub fn send(&self, bytes: impl Into<Vec<u8>>) -> anyhow::Result<MessageStatus> {
        let signal = self.ws_client.binary(bytes)?;
        Ok(signal.status())
    }
}

#[tokio::main]
async fn main() -> Result<(), DeepgramError> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vad_deepgram=debug,ezsockets=debug".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_line_number(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_target(true)
                .with_span_events(FmtSpan::CLOSE),
        )
        .init();

    let stt = STT::new().await.unwrap();
    let stt = stt.clone();

    let send_task = tokio::spawn(send_to_deepgram(stt));

    send_task.await.unwrap()?;
    info!("Done!");
    Ok(())
}

async fn send_to_deepgram(stt: STT) -> Result<(), DeepgramError> {
    
    let mut vad = VAD::new(480000, 1, 200).unwrap();

    let audio_data = tokio::fs::read(PATH_TO_FILE).await.unwrap();

    const SLICE_SIZE: usize = 480;

    let mut c = stream::iter(audio_data.chunks(SLICE_SIZE));

    // Simulate an audio stream by sending the contents of a file in chunks
    while let Some(slice) = c.next().await {
        if vad.is_talking(slice) {
            // info!("is talking!");
            if let Err(e) = stt.send(slice) {
                error!("Error sending to Deepgram: {}", e);
            };
        }else{
            info!("not talking!");
        }
        sleep(Duration::from_millis(200)).await;
    }

    warn!("DONE");

    // Tell Deepgram that we've finished sending audio data by sending a zero-byte message
    // if let Err(e) = stt.send([]) {
    //     error!("Error sending to Deepgram: {}", e);
    // };

    Ok(())
}


#[allow(clippy::upper_case_acronyms)]
struct VAD {
    inner: VoiceActivityDetector,
    sample_converter: Samplerate,
    #[allow(dead_code)]
    chunk_size: usize,
    prev: bool,
    delay: usize,
    silence_clock: Instant
}

unsafe impl Send for VAD {}

impl VAD {
    pub fn new(src_sample_rate: u32, num_of_channels: usize, delay: usize) -> anyhow::Result<Self>{
        let chunk_size = 512;
        let inner = VoiceActivityDetector::builder()
        .chunk_size(chunk_size)
        .sample_rate(16000)
        .build()?;
        let sample_converter = Samplerate::new(ConverterType::SincBestQuality, src_sample_rate, 16000, num_of_channels)?;

        Ok(
            Self {
                inner,
                sample_converter,
                chunk_size,
                delay,
                prev: false,
                silence_clock: Instant::now()
            }
        )
    }

    fn u8_to_f32_vec(v: &[u8]) -> Vec<f32> {
        v.array_chunks::<4>()
            .copied()
            .map(f32::from_le_bytes)
            .collect()
    }

    #[tracing::instrument(skip_all)]
    pub fn is_talking(&mut self, slice:& [u8]) -> bool {
        let resampled = self.sample_converter.process(&Self::u8_to_f32_vec(slice)).unwrap();
        let talking_status = self.inner.predict(resampled) > 0.75;

        let prev_temp = self.prev;
        
        self.prev = talking_status;
        if talking_status {
            // stop timer
            self.silence_clock = Instant::now();
            return talking_status;
        }
        
        // not talking

        // but was talking 
        if prev_temp {
            self.silence_clock = Instant::now();
        }
        // was not talking 

        self.silence_clock.elapsed().as_millis() as usize <= self.delay
    }
}
