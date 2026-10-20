# Contracts

Versioned capability contracts and, later, their conformance tests. A module that implements a
contract can be written in any language and under any license: this directory is
Apache-2.0 (see `LICENSE`). See [docs/16_platform.md](../docs/16_platform.md) §2–3.

| Contract | What | Implemented by |
|---|---|---|
| [`media.transcribe@1`](media.transcribe/1/README.md) | Speech to text | `modules/stt-openai` |
| [`agents.execute@1`](agents.execute/1/README.md) | Run an agent (Claude Code, Codex, …) on a job, resume it in the same session | `modules/runtime-docker` (M4) |
| [`notify.send@1`, `notify.update@1`](notify.send/1/README.md) | Show Inbox items to people in a channel; edit them when resolved | `modules/telegram` |
| [`backup.target@1`](backup.target/1/README.md) | Back the installation up into storages, check a restore | `modules/backup-restic` |

Each contract has a `README.md` (behaviour) and a `schema.json` (JSON Schema 2020-12 of the
requests and responses).

## Module protocol

A container module is an HTTP server on port 8080 inside its container:

- `GET /manifest` → the module's manifest (`frood-module.yaml` as JSON): `name`, `version`,
  `provides` (for example `["agents.execute@1"]`), `frood_api` (scopes its service token needs),
  `config` (JSON Schema of its settings).
- `GET /healthz` → 200 when ready.
- `POST /v1/capabilities/{capability}@{major}` → call a capability: JSON in, JSON out.

Both directions use the module's service token: the core sends it as
`Authorization: Bearer …` when it calls the module, and the module uses it to call the Frood API.
The module gets its environment from the core: `FROOD_URL` (the core's address as the module
sees it), `FROOD_MODULE` (its name in `frood.yaml`), `FROOD_TOKEN` (its service token),
`FROOD_MODULE_CONFIG` (its settings as JSON) and the secrets its manifest declares.

Errors are problem details (`application/problem+json`) with an optional `retryable` flag:

```json
{ "type": "about:blank", "title": "Bad Request", "status": 400, "detail": "audio.data_base64 is empty", "retryable": false }
```

A 5xx without `retryable` counts as "the module is unavailable" and the core retries with
backoff; a 4xx is final unless `retryable: true`.

## Versions

A change that breaks callers or implementers is a new major version (`@2`). The core supports
the current and the previous major version of every contract.
