# Build context: modules/stt-openai.
# The build stage runs on the builder's own platform and cross-compiles for the image's.
FROM --platform=$BUILDPLATFORM rust:1.99-slim-trixie AS build
ARG TARGETARCH
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; gnu=x86-64-linux-gnu; cc=x86_64-linux-gnu-gcc ;; \
      arm64) triple=aarch64-unknown-linux-gnu; gnu=aarch64-linux-gnu; cc=aarch64-linux-gnu-gcc ;; \
      *) echo "unsupported platform $TARGETARCH"; exit 1 ;; \
    esac; \
    printf '%s %s\n' "$triple" "$cc" > /target; \
    apt-get update; \
    apt-get install -y --no-install-recommends cmake; \
    if [ "$(dpkg --print-architecture)" != "$TARGETARCH" ]; then \
      apt-get install -y --no-install-recommends "gcc-$gnu" "libc6-dev-$TARGETARCH-cross"; \
      rustup target add "$triple"; \
    fi; \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN set -eux; read triple cc < /target; \
    upper=$(echo "$triple" | tr 'a-z-' 'A-Z_'); \
    export "CARGO_TARGET_${upper}_LINKER=$cc" "CC_$(echo "$triple" | tr '-' '_')=$cc"; \
    cargo build --release --locked --target "$triple"; \
    cp "target/$triple/release/frood-stt-openai" /frood-stt-openai

FROM debian:trixie-slim
# ffmpeg converts formats OpenAI does not accept (OGG/Opus voice messages).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home module
COPY --from=build /frood-stt-openai /usr/local/bin/frood-stt-openai
USER module
EXPOSE 8080
ENTRYPOINT ["frood-stt-openai"]
