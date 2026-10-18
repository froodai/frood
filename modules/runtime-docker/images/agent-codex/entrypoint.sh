#!/bin/sh
# A ChatGPT subscription arrives as the contents of auth.json (from `codex login`). Codex
# refreshes it in place, so it is written once per run and then left to Codex.
if [ -n "$CODEX_AUTH_JSON" ]; then
  mkdir -p "$HOME/.codex"
  if [ ! -f "$HOME/.codex/auth.json" ]; then
    printf '%s' "$CODEX_AUTH_JSON" > "$HOME/.codex/auth.json"
    chmod 600 "$HOME/.codex/auth.json"
  fi
fi
unset CODEX_AUTH_JSON
exec codex "$@"
