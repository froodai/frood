#!/bin/sh
# Starts a throwaway Frood with demo data for the console: a fresh database, `frood serve`
# from this checkout, a personal token in e2e/.token, and the demo knowledge from seed.ts.
#   FROOD_TEST_DATABASE_URL  admin URL of a PostgreSQL with pgvector (as for the core's tests)
#   FROOD_CONSOLE_PORT       default 18080
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
CORE=$(cd "$HERE/../.." && pwd)
: "${FROOD_TEST_DATABASE_URL:?set FROOD_TEST_DATABASE_URL, e.g. postgres://postgres:test@127.0.0.1:55433/postgres}"
PORT=${FROOD_CONSOLE_PORT:-18080}
DB=frood_console_e2e
ADMIN_URL=$FROOD_TEST_DATABASE_URL
export FROOD_CONSOLE_PORT=$PORT
export FROOD_CONSOLE_DB="${ADMIN_URL%/*}/$DB"
FROOD="$CORE/target/debug/frood"
WORK=$(mktemp -d)
cp "$HERE/frood.yaml" "$WORK/frood.yaml"
mkdir -p "$WORK/.frood" && : > "$WORK/.frood/secrets.env"

# A server from an earlier run goes first.
if [ -f "$HERE/.pid" ]; then kill "$(cat "$HERE/.pid")" 2>/dev/null || true; rm -f "$HERE/.pid"; fi
if [ -f "$HERE/.mcp-pid" ]; then kill "$(cat "$HERE/.mcp-pid")" 2>/dev/null || true; rm -f "$HERE/.mcp-pid"; fi
psql "$ADMIN_URL" -q -c "DROP DATABASE IF EXISTS $DB WITH (FORCE)" -c "CREATE DATABASE $DB"
(cd "$CORE" && cargo build -q -p frood-cli)
# The key that seals tokens connected from the console, as `frood init` makes it.
"$FROOD" backup key --out "$WORK/vault.txt" > /dev/null
echo "FROOD_VAULT_KEY=$(grep AGE-SECRET-KEY "$WORK/vault.txt")" >> "$WORK/.frood/secrets.env"
# An MCP server to connect.
node "$HERE/fake-mcp.mjs" > "$WORK/fake-mcp.log" 2>&1 &
echo $! > "$HERE/.mcp-pid"
"$FROOD" --config "$WORK/frood.yaml" serve > "$WORK/serve.log" 2>&1 &
echo $! > "$HERE/.pid"
for _ in $(seq 1 60); do curl -sf "http://127.0.0.1:$PORT/readyz" > /dev/null && break; sleep 0.5; done
"$FROOD" --config "$WORK/frood.yaml" token create --email demo@example.com --label console-e2e \
  --scopes knowledge:read,knowledge:capture,knowledge:propose,knowledge:decide,journal:read,admin > "$HERE/.token"
FROOD_URL="http://127.0.0.1:$PORT" FROOD_TOKEN=$(cat "$HERE/.token") node "$HERE/seed.ts"
echo "Frood for the console at http://127.0.0.1:$PORT (log: $WORK/serve.log, pid in e2e/.pid)"
