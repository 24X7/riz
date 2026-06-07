#!/bin/sh
# Narrated walkthrough of every endpoint riz exposes — system surface
# first (/ready, /_riz/*), then a REAL MCP wire test (raw JSON-RPC over
# HTTP), then every example handler across all three runtimes (Bun,
# Python, Rust) over HTTP, authorizer-gated HTTP, and WebSocket.
#
# vs. examples/smoke-all.sh: that script is assertion-style for CI.
# This one prints each command before running it, pretty-prints JSON
# where jq is available, and includes optional pauses between sections.
#
# Prereqs:
#   - cargo build --release   (produces target/release/{riz,echo-rust,chat-rust})
#   - bun installed           (TS handlers)
#   - python3 installed       (Python handlers)
#   - websocat installed      (WS round-trips; soft-skipped if missing)
#   - jq installed            (pretty-print; soft-skipped if missing)
#
# Knobs:
#   PAUSE=1 ./examples/demo.sh    # pause for ENTER between sections
#   NO_COLOR=1 ./examples/demo.sh # disable color
#
# Usage:
#   ./examples/demo.sh

set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/riz"
CFG="$ROOT/examples/riz.all.toml"
BASE="http://127.0.0.1:3000"

[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }
[ -r "$CFG" ] || { echo "missing $CFG" >&2; exit 1; }

# ── Pretty output ───────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m')
  CYAN=$(printf '\033[36m'); AMBER=$(printf '\033[33m'); GREEN=$(printf '\033[32m')
  RESET=$(printf '\033[0m')
else
  BOLD=''; DIM=''; CYAN=''; AMBER=''; GREEN=''; RESET=''
fi

PAUSE=${PAUSE:-0}
HAS_JQ=0; command -v jq >/dev/null 2>&1 && HAS_JQ=1
JQ() { if [ "$HAS_JQ" = "1" ]; then jq "$@"; else cat; fi; }

banner() { printf '\n%s%s━━━ %s ━━━%s\n' "$BOLD" "$AMBER" "$1" "$RESET"; }
sub()    { printf '%s%s%s\n' "$DIM" "$1" "$RESET"; }
run()    { printf '%s$ %s%s\n' "$CYAN" "$1" "$RESET"; eval "$1"; printf '\n'; }
pause()  {
  [ "$PAUSE" = "1" ] || return 0
  printf '%s[press ENTER to continue]%s ' "$DIM" "$RESET"
  read -r _
}

# ── Boot ────────────────────────────────────────────────────────────
banner "Booting riz"
sub "config: examples/riz.all.toml  (14 functions across Bun + Python + Rust)"

lsof -ti :3000 2>/dev/null | xargs -r kill -TERM 2>/dev/null || true
sleep 1

LOG=/tmp/riz-demo.log
"$BIN" --log-level warn --config "$CFG" run >"$LOG" 2>&1 &
RIZ_PID=$!
trap 'kill -TERM $RIZ_PID 2>/dev/null || true; wait $RIZ_PID 2>/dev/null || true' EXIT INT TERM

printf '%swaiting for /ready %s' "$DIM" "$RESET"
for _ in $(seq 1 30); do
  if curl -fs "$BASE/ready" >/dev/null 2>&1; then
    printf '%s✓%s\n' "$GREEN" "$RESET"
    break
  fi
  printf '.'
  sleep 1
done
sleep 2  # let bun/python/rust workers finish spawning
sub "logs streaming to $LOG  (pid $RIZ_PID)"
pause

# ── System surface ──────────────────────────────────────────────────
banner "System surface — every riz instance exposes these without config"

sub "/ready — readiness probe; 200 once the runtime has accepted bindings."
run "curl -s -w '  → HTTP %{http_code}\n' $BASE/ready"

sub "/_riz/registry — JSON manifest of every function (system + user)."
if [ "$HAS_JQ" = "1" ]; then
  run "curl -s $BASE/_riz/registry | jq -r '.functions[] | \"\\(.kind)  \\(.name)  [\\(.runtime // \"system\")]  → \\(.routes | join(\", \"))\"'"
else
  run "curl -s $BASE/_riz/registry"
fi
pause

sub "/_riz/health — per-function invocation counts, healthy flag, p50/p99."
if [ "$HAS_JQ" = "1" ]; then
  run "curl -s $BASE/_riz/health | jq '{status, version, uptime_secs, functions: [.functions[] | {name, invocations, healthy}]}'"
else
  run "curl -s $BASE/_riz/health"
fi
pause

sub "/_riz/metrics — Prometheus-style counters (truncated)."
run "curl -s $BASE/_riz/metrics | head -20"
pause

# ── Real MCP wire test ──────────────────────────────────────────────
banner "MCP wire test — /_riz/mcp speaks JSON-RPC 2.0, spec 2025-11-25"
sub "These are raw JSON-RPC calls — exactly what Claude Code / Cursor / the"
sub "MCP Inspector send over the wire. No riz-specific tooling involved."

sub "1) initialize — handshake. Server returns its capabilities + protocol version."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"demo\",\"version\":\"0.1.0\"}}}' \\
  $BASE/_riz/mcp | JQ ."
pause

sub "2) tools/list — every user function becomes an MCP tool automatically."
sub "Names + descriptions + inputSchema + outputSchema all derived from riz.toml."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}' \\
  $BASE/_riz/mcp | JQ '.result.tools[] | {name, description}'"
pause

sub "3) tools/call — invoke a tool. Returns BOTH content[] (text) AND"
sub "structuredContent (the raw Lambda response envelope, spec 2025-11-25)."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ping\",\"arguments\":{}}}' \\
  $BASE/_riz/mcp | JQ ."
pause

sub "4) GET /_riz/mcp — Streamable HTTP requires GET to return 405 + Allow: POST."
run "curl -s -i $BASE/_riz/mcp | head -5"
pause

# ── Plug into a real MCP client ─────────────────────────────────────
banner "Plug into a real MCP client"

sub "Option A — Claude Code: drop this into .mcp.json at your project root."
cat <<EOF
${DIM}{
  "mcpServers": {
    "riz": {
      "type": "http",
      "url": "${BASE}/_riz/mcp"
    }
  }
}${RESET}
EOF

echo
sub "Option B — Official MCP Inspector (web UI for any MCP server)."
cat <<EOF
${CYAN}\$ npx @modelcontextprotocol/inspector
  # then point it at: ${BASE}/_riz/mcp${RESET}
EOF

echo
sub "Option C — Built-in self-validating client (no external deps)."
run "$BIN mcp inspect 2>&1 | head -20"
pause

# ── HTTP examples ───────────────────────────────────────────────────
banner "HTTP — Bun handlers"

sub "ping — simplest possible handler."
run "curl -s $BASE/ping"

sub "accounts — GET /accounts/{id} with a path parameter + query string."
run "curl -s '$BASE/accounts/42?include=profile'"

sub "events — POST /events with a JSON body."
run "curl -s -X POST -H 'content-type: application/json' -d '{\"event\":\"login\",\"user\":\"alice\"}' $BASE/events"

sub "crud-accounts — one handler, two routes (POST creates, GET reads, DELETE removes)."
run "curl -s -X POST -H 'content-type: application/json' -d '{\"name\":\"alice\",\"plan\":\"pro\"}' $BASE/accounts"
run "curl -s $BASE/crud/1"
run "curl -s -X DELETE $BASE/crud/1"

sub "echo-bun — full Lambda envelope echoed back (truncated)."
run "curl -s '$BASE/echo-bun?status=200&name=alice' | head -c 240; echo"
pause

banner "HTTP — Python handler"
sub "echo-python — same envelope, served by the Python adapter."
run "curl -s -X POST -H 'content-type: application/json' -d '{\"hello\":\"world\"}' $BASE/echo-python | head -c 240; echo"
pause

banner "HTTP — Rust handler"
sub "echo-rust — bare cargo binary, same Lambda envelope."
run "curl -s '$BASE/echo-rust?name=alice' | head -c 240; echo"
pause

# ── Authorizers ─────────────────────────────────────────────────────
banner "Lambda authorizers (REQUEST type)"

sub "/protected — events handler, gated by auth-allow → should be HTTP 200."
run "curl -s -w '  → HTTP %{http_code}\n' -X POST -H 'content-type: application/json' -d '{\"event\":\"hello\"}' $BASE/protected"

sub "/forbidden — events handler, gated by auth-deny → should be HTTP 401."
run "curl -s -w '  → HTTP %{http_code}\n' $BASE/forbidden"
pause

# ── WebSocket ───────────────────────────────────────────────────────
banner "WebSocket — \$connect / \$default / \$disconnect across all 3 runtimes"

if ! command -v websocat >/dev/null 2>&1; then
  printf '%swebsocat not installed — skipping WS section.%s\n' "$AMBER" "$RESET"
else
  WS() {
    path=$1; msg=$2
    printf '%s$ echo %s | websocat ws://127.0.0.1:3000%s%s\n' "$CYAN" "$msg" "$path" "$RESET"
    reply=$(echo "$msg" | timeout 3 websocat "ws://127.0.0.1:3000$path" 2>/dev/null | head -1 || true)
    printf '  ← %s%s%s\n\n' "$GREEN" "$reply" "$RESET"
  }
  sub "/chat       (Bun)"     ; WS /chat        hello-bun
  sub "/chat-python (Python)" ; WS /chat-python hello-py
  sub "/chat-rust  (Rust)"    ; WS /chat-rust   hello-rs
fi
pause

# ── Final telemetry sweep ───────────────────────────────────────────
banner "Final telemetry — /_riz/health after the run"
sub "Every function should show its invocation count from the demo above."
echo

curl -s "$BASE/_riz/health" > /tmp/riz-health.json
python3 <<'PY'
import json
with open("/tmp/riz-health.json") as f:
    d = json.load(f)
print(f'  uptime: {d.get("uptime_secs", "?")}s   |   {len(d["functions"])} user functions registered')
print()
print(f'  {"name":<15}  {"invocations":<12}  {"healthy":<7}')
print(f'  {"-"*15}  {"-"*12}  {"-"*7}')
for fn in d["functions"]:
    print(f'  {fn["name"]:<15}  {fn["invocations"]:<12}  {fn["healthy"]}')
PY

printf '\n%s%s✓ demo complete.%s  Server log: %s\n' "$BOLD" "$GREEN" "$RESET" "$LOG"
