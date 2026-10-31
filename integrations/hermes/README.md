# Frood for Hermes Agent

Frood as the memory provider of [Hermes Agent](https://hermes-agent.nousresearch.com): your
decisions, commitments, rules and facts with their sources and history, the same memory Claude,
ChatGPT, Codex and Telegram see.

- **Before each turn** Hermes gets what Frood knows about your message, each item with its id
  (`[frood:…]`). Items no person has checked are marked "not verified".
- **Tools:** `frood_search` looks further, `frood_remember` saves a note.
- **Hermes' own memory** (what it writes with its built-in memory tool) goes to Frood too, as a
  note marked "from Hermes memory, not verified". Where it contradicts what Frood knows, you
  decide in Frood's Inbox. Removals stay in Hermes: taking knowledge back is your call.
- Whole conversations are never written to Frood.
- Subagents, cron runs and flushes read but do not write.
- If Frood is down, turns go on without it, and Hermes leaves it alone for two minutes after
  three failures.

## Install

1. A token on the Frood side, one that can read, take notes and propose, but not decide:

   ```bash
   frood token create --email you@example.com --label hermes --scopes knowledge:read,knowledge:capture,knowledge:propose
   ```

2. The plugin in Hermes:

   ```bash
   hermes plugins install froodai/frood/integrations/hermes
   hermes memory setup        # pick "frood": Frood's address, then the token
   ```

   Or by hand: copy this folder to `~/.hermes/plugins/frood/`, put `FROOD_TOKEN=…` into
   `~/.hermes/.env`, and set it in `~/.hermes/config.yaml`:

   ```yaml
   memory:
     provider: frood
   ```

   Frood's address defaults to `http://127.0.0.1:8080` (Frood on the same computer). Another one
   goes to `~/.hermes/frood.json` (`{"url": "https://…"}`) or `FROOD_URL`.

3. `hermes memory status` shows it active.

## Tests

Standard library only; the tests stand in for Hermes' modules and run a fake Frood:

```bash
python3 -m pytest integrations/hermes/tests
```

Apache-2.0, like Frood's other modules and SDKs.
