#!/bin/sh
# Smoke-test every example handler in examples/lambdas/.
# Boots riz with examples/riz.all.toml in the background, exercises every
# route via curl + websocat, prints the response, then tears the server down.
#
# Prereqs:
#   - cargo build --release   (produces target/release/{riz,echo-rust,chat-rust})
#   - bun installed           (TS handlers)
#   - python3 installed       (Python handlers)
#   - websocat installed      (for the three WebSocket handlers)
#
# Usage:
#   ./examples/smoke-all.sh
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/riz"
CFG="$ROOT/examples/riz.all.toml"

[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }

# Kill any prior riz on :3000
lsof -ti :3000 2>/dev/null | xargs -r kill -TERM 2>/dev/null || true
sleep 1

# Boot riz
"$BIN" --log-level warn --config "$CFG" run >/tmp/riz-smoke-all.log 2>&1 &
RIZ_PID=$!
trap 'kill -TERM $RIZ_PID 2>/dev/null || true; wait $RIZ_PID 2>/dev/null || true' EXIT INT TERM

for i in $(seq 1 30); do
  curl -fs http://127.0.0.1:3000/ready >/dev/null 2>&1 && break
  sleep 1
done
sleep 3   # let bun/python/rust workers finish spawning

section() { echo; printf "─── %s ───\n" "$1"; }

# ─── Bun HTTP ───
section "ping (Bun)"
curl -s http://127.0.0.1:3000/ping; echo

section "accounts (Bun) — GET /accounts/{id}"
curl -s 'http://127.0.0.1:3000/accounts/42?include=profile'; echo

section "events (Bun) — POST /events"
curl -s -X POST -H 'content-type: application/json' \
  -d '{"event":"login","user":"alice"}' \
  http://127.0.0.1:3000/events; echo

section "crud-accounts (Bun) — POST /accounts (creates)"
curl -s -X POST -H 'content-type: application/json' \
  -d '{"name":"alice","plan":"pro"}' \
  http://127.0.0.1:3000/accounts; echo

section "crud-accounts (Bun) — GET /crud/1"
curl -s http://127.0.0.1:3000/crud/1; echo

section "crud-accounts (Bun) — DELETE /crud/1"
curl -s -X DELETE http://127.0.0.1:3000/crud/1; echo

section "echo-bun (Bun) — GET /echo-bun?status=200"
curl -s 'http://127.0.0.1:3000/echo-bun?status=200&name=alice' | head -c 240; echo

# ─── Node.js HTTP ───
section "echo-node (Node.js) — GET /echo-node"
curl -s 'http://127.0.0.1:3000/echo-node?name=alice' | head -c 240; echo

# ─── Python HTTP ───
section "echo-python (Python) — POST /echo-python"
curl -s -X POST -H 'content-type: application/json' \
  -d '{"hello":"world"}' \
  http://127.0.0.1:3000/echo-python | head -c 240; echo

# ─── Rust HTTP ───
section "echo-rust (Rust) — GET /echo-rust"
curl -s 'http://127.0.0.1:3000/echo-rust?name=alice' | head -c 240; echo

# ─── Authorizers ───
section "protected (auth-allow) — must return 200"
curl -s -w '  HTTP %{http_code}\n' -X POST -H 'content-type: application/json' \
  -d '{"event":"hello"}' http://127.0.0.1:3000/protected

section "forbidden (auth-deny) — must return 401"
curl -s -w '  HTTP %{http_code}\n' http://127.0.0.1:3000/forbidden

# ─── WebSocket round-trips ───
WS_SEND() {  # $1 path, $2 message
  if ! command -v websocat >/dev/null 2>&1; then
    echo "  (websocat not installed — skipping WS test for $1)"
    return
  fi
  printf "  > %s\n  < " "$2"
  echo "$2" | timeout 3 websocat "ws://127.0.0.1:3000$1" 2>/dev/null | head -1
}

section "chat (Bun WS) — /chat"
WS_SEND /chat hello-bun

section "chat-python (Python WS) — /chat-python"
WS_SEND /chat-python hello-py

section "chat-rust (Rust WS) — /chat-rust"
WS_SEND /chat-rust hello-rs

# ─── Telemetry surface ───
section "MCP tools/list — every user function appears as an MCP tool"
"$BIN" mcp inspect 2>&1 | sed -n '1,10p'; echo "  ..."

section "/_riz/health — invocation counts per function"
curl -s http://127.0.0.1:3000/_riz/health > /tmp/riz-health.json
python3 <<'PY'
import json
with open("/tmp/riz-health.json") as f:
    d = json.load(f)
for fn in d["functions"]:
    print(f'  {fn["name"]:<14}  invocations={fn["invocations"]:<3}  healthy={fn["healthy"]}')
PY

echo
echo "✓ all examples exercised. Server log: /tmp/riz-smoke-all.log"
