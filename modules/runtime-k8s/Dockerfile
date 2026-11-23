# Build context: modules/runtime-k8s.
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
COPY Cargo.toml Cargo.lock frood-module.yaml ./
COPY src ./src
RUN set -eux; read triple cc < /target; \
    upper=$(echo "$triple" | tr 'a-z-' 'A-Z_'); \
    export "CARGO_TARGET_${upper}_LINKER=$cc" "CC_$(echo "$triple" | tr '-' '_')=$cc"; \
    cargo build --release --locked --target "$triple"; \
    cp "target/$triple/release/frood-runtime-k8s" /frood-runtime-k8s

FROM debian:trixie-slim
# CA roots for an API server behind a public CA; the cluster's own CA comes with the
# ServiceAccount.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home module \
    && mkdir /data && chown module /data
COPY --from=build /frood-runtime-k8s /usr/local/bin/frood-runtime-k8s
# Run state; agent sessions live in per-run PersistentVolumeClaims.
VOLUME /data
# A numeric user, so that Kubernetes can check runAsNonRoot.
# Numeric, so Kubernetes can check runAsNonRoot.
USER 10001
EXPOSE 8080
ENTRYPOINT ["frood-runtime-k8s"]
