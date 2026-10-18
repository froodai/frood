# Claude Code, unmodified, as the agent of a Frood run. The runtime passes the command line;
# the agent's home is the run's volume, where its sessions live.
FROM node:24-trixie-slim
ARG CLAUDE_CODE_VERSION=2.1.302
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && npm cache clean --force
RUN useradd --uid 10001 --create-home --home-dir /home/agent agent \
    && mkdir -p /home/agent/work && chown -R agent:agent /home/agent
USER agent
WORKDIR /home/agent/work
ENTRYPOINT ["claude"]
