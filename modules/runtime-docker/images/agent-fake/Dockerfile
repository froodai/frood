# A stand-in for Claude Code that prints stream-json without calling any model: for checking
# the runtime against real Docker for free.
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --uid 10001 --create-home --home-dir /home/agent agent \
    && mkdir -p /home/agent/work && chown -R agent:agent /home/agent
COPY claude /usr/local/bin/claude
USER agent
WORKDIR /home/agent/work
ENTRYPOINT ["claude"]
