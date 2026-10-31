# Frood for OpenClaw

Frood as the memory of [OpenClaw](https://docs.openclaw.ai): your decisions, commitments, rules
and facts with their sources and history, the same memory Claude, ChatGPT, Codex and Telegram see.

- **Before each turn** the agent gets what Frood knows about your request, each item with its id
  (`[frood:…]`). Items no person has checked are marked "not verified".
- **Memory tools** work over Frood:
  - `memory_search` searches it;
  - `memory_get` reads one piece of knowledge by its id;
  - `memory_add` saves a note marked "from OpenClaw memory, not verified". Where the note
    contradicts what Frood knows, you decide in Frood's Inbox.
- Whole conversations are never written to Frood.
- If Frood is down, turns go on without it. After three failures it is left alone for two
  minutes.

## Install

1. A token on the Frood side, one that can read, take notes and propose, but not decide:

   ```bash
   frood token create --email you@example.com --label openclaw --scopes knowledge:read,knowledge:capture,knowledge:propose
   ```

2. Build the plugin, then install it from this folder:

   ```bash
   npm ci && npm run build
   openclaw plugins install ./integrations/openclaw --force
   ```

3. Select it in the memory slot and let it see the request it recalls for, in `openclaw.json`:

   ```json5
   {
     plugins: {
       slots: { memory: "frood" },
       entries: {
         frood: {
           enabled: true,
           config: { url: "http://127.0.0.1:8080", token: "${FROOD_TOKEN}" },
           hooks: { allowConversationAccess: true }
         }
       }
     }
   }
   ```

   Memory plugins load through an exclusive slot, so installing alone does not activate it.
   Recall before a turn uses the `before_prompt_build` hook. For plugins that do not come with
   OpenClaw, that hook needs `allowConversationAccess`; without it only the tools work.
   `recall: false` turns recall off, and `recallLimit` (1–20, default 6) sets how much comes in.

4. `openclaw plugins inspect frood --runtime --json` shows the tools and the hook registered.

## Development

```bash
npm ci
npm test          # against a fake Frood
npm run check     # types
```

The plugin is typed against a small declaration of OpenClaw's plugin entry (`src/openclaw.d.ts`),
so building it does not pull the 250 MB `openclaw` package. It was checked against the real
plugin SDK of OpenClaw 2026.10.4. OpenClaw's plugin API is experimental: check it again on each
OpenClaw release.

Apache-2.0, like Frood's other modules and SDKs.
