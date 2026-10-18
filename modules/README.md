# Official modules

Out-of-process modules (containers) that implement capability contracts. Each has a
`frood-module.yaml` manifest ([docs/16_platform.md](../docs/16_platform.md) §2) and speaks the
module protocol: `GET /manifest`, `GET /healthz`, `POST /v1/capabilities/{capability}`, with the
module's service token in both directions. Apache-2.0.

| Module | Gives | Needs |
|---|---|---|
| [stt-openai](stt-openai) | `media.transcribe@1` through OpenAI or a server with the same API (model named in `config.model`); converts OGG/Opus voice messages with ffmpeg | `OPENAI_API_KEY` |
| [telegram](telegram) | Quick capture from a Telegram bot: text, voice, files; `/link <code>` links your account | `TELEGRAM_BOT_TOKEN` |
| [runtime-docker](runtime-docker) | `agents.execute@1`: runs Claude Code or Codex for agent jobs, one container per turn, a volume per run for its sessions | `host_docker: true` (the host's Docker, root on that machine) |

In `frood.yaml`:

```yaml
modules:
  stt:
    build: ../modules/stt-openai        # or image: ghcr.io/froodai/stt-openai:<version>
    secrets: [OPENAI_API_KEY]
    config: { model: gpt-4o-mini-transcribe }
  telegram:
    build: ../modules/telegram
    secrets: [TELEGRAM_BOT_TOKEN]
    config: { language: ru }
```

Then `frood apply`, and `frood link telegram --email you@example.com` gives the code to send
to the bot.

### Agents

```yaml
modules:
  agents:
    build: ../modules/runtime-docker
    host_docker: true                   # it starts containers on this machine's Docker
    config:
      agents:
        claude-code: { image: frood-agent-claude-code }   # docker build -t frood-agent-claude-code modules/runtime-docker/images/agent-claude-code
      cpus: 1
      memory_mb: 2048

sources:
  team-chat: { uses: mcp, server: 'https://mcp.example.com/mcp', headers: { Authorization: 'Bearer ${secret:CHAT_MCP_TOKEN}' } }

jobs:
  daily-sweep:
    on: [{ schedule: '0 3 * * *', timezone: Europe/Amsterdam, for_each: source }]
    runtime: agents
    agent: claude-code
    model: claude-haiku-5-5
    account: anthropic
    budget: { max_usd: 0.5, max_minutes: 15 }
```

Each turn of the agent is a fresh container from the agent's image: no capabilities, no new
privileges, a read-only root, CPU and memory limits, and only the run's volume as its home. The
agent gets the run's MCP servers (Frood with a run token, the run's source) and no built-in tools
unless `builtin_tools` names some. `images/agent-fake` is a stand-in that prints Claude Code's
output without calling a model, for checking a deployment for free.

