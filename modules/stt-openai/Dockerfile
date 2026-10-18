# Build context: modules/stt-openai.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:trixie-slim
# ffmpeg converts formats OpenAI does not accept (OGG/Opus voice messages).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home module
COPY --from=build /src/target/release/frood-stt-openai /usr/local/bin/frood-stt-openai
USER module
EXPOSE 8080
ENTRYPOINT ["frood-stt-openai"]
