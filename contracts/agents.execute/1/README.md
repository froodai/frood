# `agents.execute@1`

Runs a coding or research agent (Claude Code, Codex, …) on a Frood job, follows it, and resumes
it **in the same session** after the agent asked people something. The agent works with Frood's
memory only through the MCP servers it is given. See [docs/13](../../../docs/13_work_agents.md)
§5–7 and [docs/21](../../../docs/21_m4_plan.md).

One endpoint, `POST /v1/capabilities/agents.execute@1`, with an `op` in the body. Every request
names the run (`run_id`, a UUID chosen by the core).

## Lifecycle

```
start ──▶ running ──▶ finished ──(core: no open questions)──▶ succeeded
              │            └──(core: open questions)──▶ waiting_for_input ──answers──▶ resume ──▶ running …
              ├──▶ failed
              └── stop (budget, cancel, expired question)
```

The runtime knows only `running`, `finished` and `failed`. Waiting for people, budgets and
cancelling are the core's business: when the agent ends its turn after calling `ask_user`, the
runtime reports `finished` and the core parks the run until the questions are settled, then
sends `resume`.

## Operations

### `start`

```json
{
  "op": "start",
  "run_id": "0199f1c2-…",
  "job": "daily-sweep",
  "agent": "claude-code",
  "model": "claude-haiku-5-5",
  "instructions": "You are Frood's nightly sweep for one source…",
  "prompt": "You are running the Frood job `daily-sweep` (run …). What started this run: …",
  "mcp_servers": [
    { "name": "frood", "path": "/mcp", "headers": { "Authorization": "Bearer frood_run_…" } },
    { "name": "source-youtrack", "url": "https://youtrack.example.com/mcp", "headers": { "Authorization": "Bearer …" }, "tools": ["search_issues", "get_issue"] }
  ],
  "credentials": { "provider": "anthropic", "kind": "api_key", "api_key": "sk-ant-…", "workspace": "wrkspc_…" },
  "budget": { "max_usd": 0.5, "max_minutes": 15 },
  "ask_user": true
}
```

→ `{ "session_ref": "…", "state": "running" }`

- `agent` names the agent program; a runtime lists the agents it has in its settings and
  rejects others with 400.
- `instructions` are the system prompt (appended to the agent's own); `prompt` is the first user
  message.
- `mcp_servers` are the only MCP servers the agent gets. Names are letters, digits, `_` and
  `-` (agents name tools after them). `path` is relative to `FROOD_URL`; `url` is absolute.
  `tools`, when present, are the only tools of that server the agent may call: a source the
  agent must not change lists only its reading tools. Pass `headers` as they are. The `frood` server's token is a **run token**:
  it lives as long as the run, may read, capture and propose, never decide.
- `credentials` pay for the run, in one of two kinds:
  - `{ "kind": "api_key", "provider", "api_key", "workspace" }` — an API key; `workspace` is
    set for Anthropic organization keys (send it as `anthropic-workspace-id`, or the agent's
    equivalent);
  - `{ "kind": "subscription", "provider", "token" }` — a person's subscription, only for that
    person's runs (the core checks). Anthropic: the token from `claude setup-token`, used as
    `CLAUDE_CODE_OAUTH_TOKEN` of an unmodified Claude Code. OpenAI: the contents of
    `~/.codex/auth.json` after `codex login`, for Codex. Never use it for anything but that
    agent.

  They are for this run only. Do not write them where another run, or the agent's own tools,
  can read them later, and never log them or the headers.
- `budget` is a limit the runtime should pass to the agent when it supports one. The core also
  watches `cost_usd` and minutes and sends `stop` when the run is over.
- `session_ref` is the agent's own session id, kept so that `resume` continues the same
  conversation. It may be `null` when the agent names its session only after it starts; then
  `status` reports it. Sessions must survive a restart of the runtime (keep them on a volume).

### `resume`

The same fields as `start` without `prompt`, plus:

```json
{ "op": "resume", "session_ref": "…", "message": "Answers to your questions:\n- Decision or discussion?\n  Decision" }
```

→ `{ "session_ref": "…", "state": "running" }`

`mcp_servers` carries a **new** run token: the old one is revoked before `resume`. `message`
also carries people's messages sent while the run waited.

### `send`

`{ "op": "send", "run_id": "…", "message": "Also look at #release" }` → `{ "delivered": true }`

A person's message to a running agent. Return `delivered: false` when the agent cannot take
input now; the core keeps the message and, when the agent ends its turn, resumes the session
with it.

### `stop`

`{ "op": "stop", "run_id": "…" }` → `{}`

Ends the agent and frees what it holds, its session included. The core sends it whenever a run
ends: succeeded, failed, cancelled or over its budget. Idempotent; stopping an unknown or ended
run is not an error.

### `status`

`{ "op": "status", "run_id": "…" }` →

```json
{ "state": "finished", "session_ref": "…", "cost_usd": 0.0123, "result": "Read 40 messages, proposed 2 changes: …", "error": null }
```

- `state`: `running`, `finished` (the agent ended its turn normally) or `failed` (it crashed,
  could not start, or ran out of budget on its own; `error` says why).
- `cost_usd`: what the run has cost so far across all its sessions, as the agent reports it.
- `result`: the agent's last answer when `finished`.

### `events`

`{ "op": "events", "run_id": "…", "after": 3 }` → `{ "events": [{ "kind": "tool_use", "data": { … } }, …] }`

What happened inside the run, numbered from 1 across the whole run (all its sessions); `after: n`
returns events n+1 onwards. The core polls it every few seconds and stores the events in the
run's log. Suggested kinds: `assistant` (`{text}`), `tool_use` (`{name}`), `tool_result`
(`{name, is_error}`), `usage` (`{input_tokens, output_tokens, cost_usd}`), `error` (`{message}`).
Keep `data` small and free of secrets.

## Errors

Problem details as in the [module protocol](../../README.md#module-protocol). An unknown
`run_id` for `resume`, `send`, `status` or `events` is 404. A runtime that cannot start an agent
right now (no capacity, the image is pulling) answers 503 with `retryable: true`; the core retries
with backoff.

## Conformance

The core's expectations are exercised against a fake runtime in
[`core/crates/frood-api/tests/agents.rs`](../../../core/crates/frood-api/tests/agents.rs);
a reusable conformance suite for runtimes comes with the first one (`modules/runtime-docker`).
