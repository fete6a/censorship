FROM rust:1.91-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/telegram-audio-transcoder /usr/local/bin/audio-transcoder
USER 65532:65532
EXPOSE 8788
CMD ["/usr/local/bin/audio-transcoder"]
