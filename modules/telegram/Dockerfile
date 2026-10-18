# Build context: modules/telegram.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home module
COPY --from=build /src/target/release/frood-telegram /usr/local/bin/frood-telegram
USER module
EXPOSE 8080
ENTRYPOINT ["frood-telegram"]
