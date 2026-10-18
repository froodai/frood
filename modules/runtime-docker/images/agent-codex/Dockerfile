# Codex CLI as the agent of a Frood run.
FROM node:24-trixie-slim
ARG CODEX_VERSION=0.163.0
RUN npm install -g @openai/codex@${CODEX_VERSION} && npm cache clean --force
RUN useradd --uid 10001 --create-home --home-dir /home/agent agent \
    && mkdir -p /home/agent/work && chown -R agent:agent /home/agent
COPY entrypoint.sh /usr/local/bin/agent-entrypoint
USER agent
WORKDIR /home/agent/work
ENTRYPOINT ["agent-entrypoint"]
