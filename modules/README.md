# Official modules

Out-of-process modules (containers) that implement capability contracts. Each has a
`frood-module.yaml` manifest ([docs/16_platform.md](../docs/16_platform.md) §2) and speaks the
module protocol: `GET /manifest`, `GET /healthz`, `POST /v1/capabilities/{capability}`, with the
module's service token in both directions. Apache-2.0.

| Module | Gives | Needs |
|---|---|---|
| [stt-openai](stt-openai) | `media.transcribe@1` through OpenAI or a server with the same API (model named in `config.model`); converts OGG/Opus voice messages with ffmpeg | `OPENAI_API_KEY` |
| [telegram](telegram) | Quick capture from a Telegram bot: text, voice, files; `/link <code>` links your account | `TELEGRAM_BOT_TOKEN` |
| [notify-openclaw](notify-openclaw) | The Inbox and the digest in an OpenClaw conversation (WhatsApp, iMessage, Signal…): text with numbered choices, a reply with the number decides | an OpenClaw Gateway, `OPENCLAW_GATEWAY_TOKEN` |
| [runtime-docker](runtime-docker) | `agents.execute@1`: runs Claude Code or Codex for agent jobs, one container per turn, a volume per run for its sessions | `host_docker: true` (the host's Docker, root on that machine) |
| [backup-restic](backup-restic) | `backup.target@1`: the installation into restic repositories (a folder, S3, B2, SFTP, REST), and a restore check into a temporary database | the targets in `backup`; `volumes` for a folder target |

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

### OpenClaw

```yaml
modules:
  openclaw:
    image: ghcr.io/froodai/notify-openclaw:<version>
    secrets: [OPENCLAW_GATEWAY_TOKEN]
    config: { gateway: 'ws://host.docker.internal:18789', language: ru }
```

The module starts `openclaw mcp serve` and connects to the OpenClaw Gateway, so the Gateway must
be reachable from the container. A Gateway that listens only on 127.0.0.1 of a Linux machine is
not: give it an address the container can reach, or a `wss://` one. `frood link openclaw --email
you@example.com` gives a code; send `frood <code>` in the conversation where you want the Inbox,
which also gives it the route OpenClaw needs to write there. Only the conversation owner's
messages count: a reply decides for the person linked, and in a group nobody else can.

OpenClaw's conversations carry text only, with no buttons and no edits. So items arrive with
numbered choices, a reply with the number (or yes, or no, where it is clear) takes one, and what
is decided elsewhere comes as a new message. The agent sees these replies too, as any message
in the conversation. Which items wait for a reply is kept in memory: after a restart of the
module, answer those in the console. Events that come while the Gateway is away are not seen.

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


### Backups

```yaml
modules:
  backup:
    build: ../modules/backup-restic
    volumes: ["/Volumes/Backup/frood:/repos/disk"]   # a folder target: a disk the module sees

backup:
  recovery_key: age1…                 # `frood backup key --out recovery-key.txt`; keep that file off the server
  targets:
    disk: { repository: /repos/disk, password: '${secret:BACKUP_DISK_PASSWORD}' }
    b2:
      repository: 's3:https://s3.eu-central-003.backblazeb2.com/my-frood-backups'
      password: '${secret:BACKUP_B2_PASSWORD}'
      env: { AWS_ACCESS_KEY_ID: '${secret:B2_KEY_ID}', AWS_SECRET_ACCESS_KEY: '${secret:B2_KEY}' }
  keep: { daily: 30, monthly: 12 }

jobs:
  backup: { on: [{ schedule: '0 */6 * * *', timezone: Europe/Amsterdam }] }
  backup-verify: { on: [{ schedule: '30 4 * * 0', timezone: Europe/Amsterdam }] }
```

Each backup is one consistent dump of the database with the row count of every table, plus
`frood.yaml` and the secrets file encrypted to the recovery key. `backup-verify` restores each
target's latest backup into a temporary database next to the real one and compares the counts.
The admin panel shows both and has buttons to run them now; an alert comes when backups stop or a
check fails.

On a clean machine, in an empty folder:

```sh
RESTIC_PASSWORD=… frood restore --repository /Volumes/Backup/frood --identity recovery-key.txt \
  --image ghcr.io/froodai/backup-restic:0.1.0
frood apply
```

For S3 or B2 add `--env AWS_ACCESS_KEY_ID=… --env AWS_SECRET_ACCESS_KEY=…`.
