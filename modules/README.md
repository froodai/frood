# Official modules

Out-of-process modules (containers) that implement capability contracts. Each has a
`frood-module.yaml` manifest ([docs/16_platform.md](../docs/16_platform.md) §2) and speaks the
module protocol: `GET /manifest`, `GET /healthz`, `POST /v1/capabilities/{capability}`, with the
module's service token in both directions. Apache-2.0.

| Module | Gives | Needs |
|---|---|---|
| [stt-openai](stt-openai) | `media.transcribe@1` through OpenAI (`gpt-transcribe` by default); converts OGG/Opus voice messages with ffmpeg | `OPENAI_API_KEY` |
| [telegram](telegram) | Quick capture from a Telegram bot: text, voice, files; `/link <code>` links your account | `TELEGRAM_BOT_TOKEN` |

In `frood.yaml`:

```yaml
modules:
  stt:
    build: ../modules/stt-openai        # or image: ghcr.io/froodai/stt-openai:<version>
    secrets: [OPENAI_API_KEY]
  telegram:
    build: ../modules/telegram
    secrets: [TELEGRAM_BOT_TOKEN]
    config: { language: ru }
```

Then `frood apply`, and `frood link telegram --email you@example.com` gives the code to send
to the bot.
