# `notify.send@1` and `notify.update@1`

A channel module (Telegram, Slack, push, mail…) shows Inbox items to people and edits what it
showed when the situation is resolved. See [docs/15](../../../docs/15_inbox_attention.md) and
[docs/22](../../../docs/22_m5_plan.md).

The core decides who gets what and when (routing, quiet hours, limits); the module decides how
it looks in its channel and in which language. People's answers come back through the Frood API
(`POST /v1/inbox/{id}/act`), called by the module with its service token (`channels:act`) and the
person's channel account in `external_id`: the action is that linked person's
(`user:<id>/via/<channel>`).

## `notify.send@1`

`POST /v1/capabilities/notify.send@1`

```json
{
  "recipient": { "user_id": "0199…", "external_id": "373739001" },
  "item": {
    "item": { "id": "0199…", "kind": "proposal", "urgency": "now", "state": "open", "investigation": null, "…": "…" },
    "content": {
      "risk": "conflict",
      "author": "agent:daily-sweep/for/0199…",
      "rationale": "#release, Olya, 18:02",
      "quote": "релиз переносим на 28.11",
      "alternatives": [
        { "key": "default", "label": null, "changes": [ { "kind": "commitment", "before": "Релиз 2.4 — 14 ноября", "after": "Релиз 2.4 — 28 ноября" } ] }
      ]
    },
    "actions": [
      { "action": "accept", "alternative": "default", "label": null },
      { "action": "reject" },
      { "action": "defer" }
    ]
  }
}
```

→ `{ "message_ref": "373739001:52" }`

- `recipient.external_id` is the person's account in this channel, from the account link.
- `item.item.kind` is `proposal`, `question`, `reminder`, `alert` or `digest`; `content` depends on
  it:
  - `proposal`: `risk`, `author`, `rationale`, `quote` (the words it rests on), `alternatives`
    with `changes` (`before` / `after` texts; `before` is null for new knowledge, `after` is null
    for a closing);
  - `question`: `text`, `options`, `context`, `job`;
  - `reminder`: `assertion_id`, `text`, `due`, `step` (`before`, `today`, `overdue`);
  - `alert`: `run_id`, `job`, `error`;
  - `digest`: `since`, `sections`.
- `item.item.investigation`, when present, carries an agent's `report` gathered before anyone was
  told: show it with the item.
- `actions` are what the person may do; each becomes a button. Send the chosen one back as the
  body of `POST /v1/inbox/{id}/act` (`action` plus its `alternative` or `answer`), with
  `external_id`.
- `console_url`, when the installation has a public address, opens the item in the console:
  a "details" link for the message. Optional; absent on a local installation.
- A decision can be taken back: `POST /v1/inbox/{id}/undo` with `external_id` opens the item
  again (an "undo" button after a decision).
- `message_ref` is whatever lets the module edit the message later.

## `notify.update@1`

`POST /v1/capabilities/notify.update@1`

```json
{
  "recipient": { "user_id": "0199…", "external_id": "373739001" },
  "message_ref": "373739001:52",
  "item": { "item": { "state": "resolved", "resolution": { "action": "accept", "by": "user:0199…/via/claude-code", "at": "…" }, "…": "…" }, "content": { "…": "…" }, "actions": [] }
}
```

→ `{}`

The situation was resolved — maybe in another channel or client. Edit the message to say how and
by whom, and remove its buttons. An item the module cannot find any more is not an error.

## Errors

Problem details as in the [module protocol](../../README.md#module-protocol). A recipient who
blocked the bot or deleted the chat is a 4xx without `retryable`: the core stops trying.
