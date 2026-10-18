# Build context: modules/runtime-docker.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock frood-module.yaml ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/frood-runtime-docker /usr/local/bin/frood-runtime-docker
# Run state; agent sessions live in per-run Docker volumes.
VOLUME /data
EXPOSE 8080
# Runs as root: it drives the host's Docker, which is root on that machine anyway (A2).
ENTRYPOINT ["frood-runtime-docker"]
