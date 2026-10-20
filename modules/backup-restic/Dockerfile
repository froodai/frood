# Build context: modules/backup-restic.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

# pg_dump and pg_restore must match the server's major version: the same PostgreSQL 18 image.
FROM postgres:18-trixie
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates restic \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home --home-dir /home/module module \
    && mkdir -p /backup /restore && chown module /backup /restore
COPY --from=build /src/target/release/frood-backup-restic /usr/local/bin/frood-backup-restic
USER module
ENV HOME=/home/module
EXPOSE 8080
ENTRYPOINT ["frood-backup-restic"]
