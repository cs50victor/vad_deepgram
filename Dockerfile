# Chef
FROM lukemathwalker/cargo-chef:latest as chef
# RUN rustup toolchain install nightly
RUN rustup override set nightly
WORKDIR /app

# Plan
FROM chef AS planner
COPY . .
RUN cargo chef prepare

# Cook and Build
FROM chef AS builder
COPY --from=planner /app/recipe.json .
RUN apt-get update
# cmake (sample rate crate)
RUN apt-get install cmake --no-install-recommends -y
RUN cargo +nightly chef cook --release
COPY . .
RUN cargo build --release
RUN mv ./target/release/vad_deepgram .

FROM debian:stable-slim AS runtime
WORKDIR /app
COPY --from=builder /app/vad_deepgram .
COPY --from=builder /app/mkbhd_video.mp3 .
EXPOSE 8080
ENTRYPOINT ["./vad_deepgram"]


