# Image of the frood binary (server + CLI). Build context: core/.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p frood-cli

FROM debian:trixie-slim
# CA roots: the HTTP client verifies TLS against the system store (OIDC providers, CIMD).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home frood
COPY --from=build /src/target/release/frood /usr/local/bin/frood
USER frood
EXPOSE 8080
ENTRYPOINT ["frood"]
CMD ["serve", "--config", "/etc/frood/frood.yaml"]
