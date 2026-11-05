# Build context: modules/notify-openclaw.
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
    cp "target/$triple/release/frood-notify-openclaw" /frood-notify-openclaw

# The module runs `openclaw mcp serve`, so the image carries OpenClaw itself (Node 24).
FROM node:24-trixie-slim
ARG OPENCLAW_VERSION=2026.10.5
RUN npm install --global --ignore-scripts --no-audit --no-fund "openclaw@$OPENCLAW_VERSION" \
    && npm cache clean --force
# What the module relies on: a release that drops these flags fails here, not in someone's chat.
RUN openclaw mcp serve --help | tee /tmp/help \
    && grep -q -- --token-file /tmp/help \
    && grep -q -- --claude-channel-mode /tmp/help \
    && rm /tmp/help
RUN useradd --system --uid 10001 --create-home module
COPY --from=build /frood-notify-openclaw /usr/local/bin/frood-notify-openclaw
USER module
EXPOSE 8080
ENTRYPOINT ["frood-notify-openclaw"]
