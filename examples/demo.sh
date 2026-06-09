#!/bin/sh
# ╔══════════════════════════════════════════════════════════════════════╗
# ║  riz — complete narrated walkthrough of EVERY capability.             ║
# ╚══════════════════════════════════════════════════════════════════════╝
#
# Boots one riz instance from examples/riz.all.toml and demonstrates, live:
#   • System surface          /ready, /_riz/registry, /_riz/health, /_riz/metrics
#   • MCP server (2025-11-25)  raw JSON-RPC wire test + built-in inspector
#   • All FOUR runtimes        Bun, Node.js, Python, Rust — same Lambda envelope
#   • HTTP shapes              path params, query string, JSON body, all verbs (CRUD)
#   • Response caching         cache HIT replays response; POST /cache/invalidate evicts
#   • CORS                     OPTIONS preflight + Access-Control-Allow-Origin
#   • Stage variables          per-function config surfaced on the event
#   • On-box safety            per-function rlimit/Landlock caps + always-on profile
#   • Lambda authorizers       REQUEST allow → 200, deny → 401
#   • WebSocket                $connect/$default/$disconnect across Bun/Python/Rust
#   • Handler hot-reload        edit a handler, next request runs new code (no restart)
#   • Scaffolding & doctor      riz init --list, riz doctor
#
# This is the SHOWCASE script (prints each command, pretty-prints JSON).
# examples/smoke-all.sh is the terse assertion-style CI companion.
#
# Prereqs:
#   - cargo build --release   (produces target/release/{riz,echo-rust,chat-rust})
#   - bun, node, python3 on PATH   (TS / JS / Python handlers)
#   - websocat   (WS round-trips; soft-skipped if missing)
#   - jq         (pretty-print; soft-skipped if missing)
#
# Knobs:
#   PAUSE=1 ./examples/demo.sh     # pause for ENTER between sections
#   NO_COLOR=1 ./examples/demo.sh  # disable color
#
# Usage:  ./examples/demo.sh

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
  RED=$(printf '\033[31m'); RESET=$(printf '\033[0m')
else
  BOLD=''; DIM=''; CYAN=''; AMBER=''; GREEN=''; RED=''; RESET=''
fi

PAUSE=${PAUSE:-0}
HAS_JQ=0; command -v jq >/dev/null 2>&1 && HAS_JQ=1
JQ() { if [ "$HAS_JQ" = "1" ]; then jq "$@"; else cat; fi; }

banner() { printf '\n%s%s━━━ %s ━━━%s\n' "$BOLD" "$AMBER" "$1" "$RESET"; }
sub()    { printf '%s%s%s\n' "$DIM" "$1" "$RESET"; }
ok()     { printf '  %s✓ %s%s\n' "$GREEN" "$1" "$RESET"; }
warn()   { printf '  %s! %s%s\n' "$AMBER" "$1" "$RESET"; }
run()    { printf '%s$ %s%s\n' "$CYAN" "$1" "$RESET"; eval "$1"; printf '\n'; }
pause()  {
  [ "$PAUSE" = "1" ] || return 0
  printf '%s[press ENTER to continue]%s ' "$DIM" "$RESET"
  read -r _
}

# ── Boot ────────────────────────────────────────────────────────────
banner "Booting riz"
sub "config: examples/riz.all.toml  (15 functions across Bun · Node.js · Python · Rust)"

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
sleep 2  # let bun/node/python/rust workers finish spawning
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

sub "/_riz/health — per-function invocation counts, healthy flag, latency."
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
sub "Raw JSON-RPC — exactly what Claude Code / Cursor / the MCP Inspector send."

sub "1) initialize — handshake; server returns capabilities + protocol version."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"demo\",\"version\":\"0.1.0\"}}}' \\
  $BASE/_riz/mcp | JQ ."
pause

sub "2) tools/list — every user function becomes an MCP tool automatically."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}' \\
  $BASE/_riz/mcp | JQ '.result.tools[] | {name, description}'"
pause

sub "3) tools/call — invoke the ping tool over MCP."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ping\",\"arguments\":{}}}' \\
  $BASE/_riz/mcp | JQ ."
pause

sub "4) Built-in self-validating client — no external deps."
run "$BIN mcp inspect 2>&1 | head -20"
pause

# ── LLM gateway / OpenAI-compatible API ─────────────────────────────
banner "LLM gateway — OpenAI-compatible API at /_riz/v1"
sub "Point any OpenAI client at /_riz/v1. The 'mock' provider is deterministic"
sub "and network-free, so this runs with no API key. Real providers ship too —"
sub "add [gateway.providers.*] kind=openai/ollama; requests route + fall back."

sub "GET /_riz/v1/models — configured providers."
run "curl -s $BASE/_riz/v1/models | JQ ."

sub "POST /_riz/v1/chat/completions — the OpenAI chat-completions shape."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"model\":\"mock\",\"messages\":[{\"role\":\"user\",\"content\":\"hello riz\"}],\"stream\":false}' \\
  $BASE/_riz/v1/chat/completions | JQ '{model, message: .choices[0].message, usage}'"

sub "stream=true — Server-Sent Events (chat.completion.chunk … then [DONE])."
run "curl -sN -X POST -H 'content-type: application/json' \\
  -d '{\"model\":\"mock\",\"messages\":[{\"role\":\"user\",\"content\":\"stream me\"}],\"stream\":true}' \\
  $BASE/_riz/v1/chat/completions | head -7"

sub "POST /_riz/v1/embeddings — OpenAI embeddings shape (deterministic mock vectors)."
run "curl -s -X POST -H 'content-type: application/json' \\
  -d '{\"model\":\"mock\",\"input\":[\"the quick brown fox\",\"lorem ipsum\"]}' \\
  $BASE/_riz/v1/embeddings | JQ '{object, model, count: (.data | length), dims: (.data[0].embedding | length), usage}'"

sub "The OFFICIAL openai python client, pointed at riz via base_url:"
if python3 -c 'import openai' >/dev/null 2>&1; then
  printf '%s$ OpenAI(base_url="%s/_riz/v1").chat.completions.create(model="mock", ...)%s\n' "$CYAN" "$BASE" "$RESET"
  RIZ_OPENAI_BASE="$BASE/_riz/v1" python3 - <<'PY'
import os
from openai import OpenAI
c = OpenAI(base_url=os.environ["RIZ_OPENAI_BASE"], api_key="not-needed")
r = c.chat.completions.create(
    model="mock",
    messages=[{"role": "user", "content": "hi from the openai client"}],
    stream=False,
)
print("  ←", r.choices[0].message.content)
PY
else
  warn "openai python package not installed (pip install openai) — the curl above is the exact wire format it speaks."
fi
pause

# ── HTTP across all four runtimes ───────────────────────────────────
banner "HTTP — four runtimes, one Lambda envelope"

sub "ping (Bun) — simplest possible handler."
run "curl -s $BASE/ping"

sub "echo-node (Node.js) — the #1 production Lambda runtime."
run "curl -s '$BASE/echo-node?name=alice' | JQ '{echo, method, functionName, awsRequestId}'"

sub "echo-python (Python) — same envelope, Python adapter."
run "curl -s -X POST -d '{\"hello\":\"world\"}' $BASE/echo-python | JQ '{echo, method, functionName}'"

sub "echo-rust (Rust) — bare cargo binary, same envelope."
run "curl -s '$BASE/echo-rust?name=alice' | JQ '{echo, method, functionName}'"
pause

# ── HTTP request shapes ─────────────────────────────────────────────
banner "HTTP request shapes (Bun)"

sub "accounts — GET /accounts/{id} with a path parameter + query string."
run "curl -s '$BASE/accounts/42?include=profile' | JQ ."

sub "events — POST /events with a JSON body."
run "curl -s -X POST -H 'content-type: application/json' -d '{\"event\":\"login\",\"user\":\"alice\"}' $BASE/events | JQ ."

sub "crud-accounts — one handler, all five verbs (POST creates, GET reads, DELETE removes)."
run "curl -s -X POST -H 'content-type: application/json' -d '{\"name\":\"alice\",\"plan\":\"pro\"}' $BASE/accounts | JQ '{id, name, plan}'"
run "curl -s -w '  → HTTP %{http_code}\n' $BASE/crud/1 -o /dev/null"
run "curl -s -w '  → HTTP %{http_code}\n' -X DELETE $BASE/crud/1 -o /dev/null"
pause

# ── Response caching ────────────────────────────────────────────────
banner "Response caching — GET /accounts/{id} cached 30s, then invalidate"
sub "The handler stamps a 'ts' into every response. A cache HIT replays the"
sub "same ts (handler not re-run); POST /cache/invalidate evicts and the next"
sub "request stamps a fresh ts."

TS1=$(curl -s "$BASE/accounts/7" | JQ -r '.ts')
TS2=$(curl -s "$BASE/accounts/7" | JQ -r '.ts')
printf '  request 1 ts = %s\n  request 2 ts = %s  ' "$TS1" "$TS2"
if [ "$TS1" = "$TS2" ]; then ok "identical → served from cache"; else warn "differ (jq missing?)"; fi

run "curl -s -X POST -H 'content-type: application/json' -d '{\"prefix\":\"GET:/accounts/\"}' $BASE/cache/invalidate"
TS3=$(curl -s "$BASE/accounts/7" | JQ -r '.ts')
printf '  request 3 ts = %s  ' "$TS3"
if [ "$TS3" != "$TS1" ]; then ok "changed → cache evicted, handler re-ran"; else warn "unchanged"; fi
pause

# ── CORS ────────────────────────────────────────────────────────────
banner "CORS — global [cors] policy (allow_origins = app.example.com)"

sub "OPTIONS preflight from an allowed origin → 204 + Access-Control-* headers."
run "curl -s -i -X OPTIONS -H 'Origin: https://app.example.com' -H 'Access-Control-Request-Method: GET' $BASE/ping | grep -iE '^HTTP|^access-control' | sed 's/^/  /'"

sub "A real request echoes Access-Control-Allow-Origin for the allowed origin."
run "curl -s -i -H 'Origin: https://app.example.com' $BASE/ping | grep -iE '^access-control-allow-origin' | sed 's/^/  /'"
pause

# ── Stage variables ─────────────────────────────────────────────────
banner "Stage variables — per-function config on the event (echo-bun)"
sub "[function.echo-bun.stage_variables] region/tier surface as event.stageVariables."
run "curl -s '$BASE/echo-bun' | JQ '.stageVariables'"
pause

# ── On-box safety ───────────────────────────────────────────────────
banner "On-box safety — per-function caps + an always-on profile"
sub "echo-python declares opt-in caps; an always-on profile applies to EVERY handler."
printf '%s' "$DIM"
sed -n '/\[function.echo-python\]/,/routes\]\]/p' "$CFG" | grep -E '^(cpu_time_secs|allowed_paths)' | sed 's/^/  /'
printf '%s' "$RESET"
ok "cpu_time_secs → RLIMIT_CPU · allowed_paths → Landlock (Linux) · memory_mb → RLIMIT_AS (opt-in)"
ok "always-on (every child): RLIMIT_CORE=0, fd/file-size caps, PDEATHSIG, NO_NEW_PRIVS"
pause

# ── Authorizers ─────────────────────────────────────────────────────
banner "Lambda authorizers (REQUEST type)"
sub "/protected — gated by auth-allow → HTTP 200."
run "curl -s -w '  → HTTP %{http_code}\n' -X POST -d '{\"event\":\"hello\"}' $BASE/protected -o /dev/null"
sub "/forbidden — gated by auth-deny → HTTP 401 (handler never runs)."
run "curl -s -w '  → HTTP %{http_code}\n' $BASE/forbidden -o /dev/null"
sub "JWT authorizers (RS256/ES256 against your IdP's JWKS — Auth0/Cognito/Okta/Keycloak)"
sub "are configured with [function.X.authorizer] type=\"jwt\" + jwks_uri/issuer/audience;"
sub "proven end-to-end in tests/wave_3_acceptance.rs. See examples/riz.jwt.toml."
pause

# ── WebSocket ───────────────────────────────────────────────────────
banner "WebSocket — \$connect / \$default / \$disconnect across 3 runtimes"
if ! command -v websocat >/dev/null 2>&1; then
  warn "websocat not installed — skipping WS round-trips (install: brew install websocat)"
else
  WS() {
    path=$1; msg=$2
    printf '%s$ echo %s | websocat ws://127.0.0.1:3000%s%s\n' "$CYAN" "$msg" "$path" "$RESET"
    reply=$(echo "$msg" | timeout 3 websocat "ws://127.0.0.1:3000$path" 2>/dev/null | head -1 || true)
    printf '  ← %s%s%s\n\n' "$GREEN" "$reply" "$RESET"
  }
  sub "/chat        (Bun)"    ; WS /chat        hello-bun
  sub "/chat-python (Python)" ; WS /chat-python hello-py
  sub "/chat-rust   (Rust)"   ; WS /chat-rust   hello-rs
fi
pause

# ── Handler hot-reload ──────────────────────────────────────────────
banner "Handler hot-reload — edit a handler, next request runs new code (no restart)"
PING_TS="$ROOT/examples/lambdas/ping/index.ts"
PING_BAK="$(mktemp)"
cp "$PING_TS" "$PING_BAK"
# Ensure the original is restored no matter how the script exits.
trap 'kill -TERM $RIZ_PID 2>/dev/null || true; wait $RIZ_PID 2>/dev/null || true; cp "$PING_BAK" "$PING_TS" 2>/dev/null || true; rm -f "$PING_BAK"' EXIT INT TERM

sub "before:"
run "curl -s $BASE/ping"
sub "editing examples/lambdas/ping/index.ts:  status \"ok\" → \"hot-reloaded\""
sed -i.swp 's/status: "ok"/status: "hot-reloaded"/' "$PING_TS" && rm -f "$PING_TS.swp"
printf '%swaiting for the source watcher to hot-swap the pool %s' "$DIM" "$RESET"
for _ in $(seq 1 20); do
  if curl -s "$BASE/ping" | grep -q 'hot-reloaded'; then printf '%s✓%s\n' "$GREEN" "$RESET"; break; fi
  printf '.'; sleep 0.5
done
sub "after (no restart — same pid $RIZ_PID):"
run "curl -s $BASE/ping"
cp "$PING_BAK" "$PING_TS"; rm -f "$PING_BAK"
ok "ping/index.ts restored to its original contents"
pause

# ── Scaffolding & diagnostics ───────────────────────────────────────
banner "Scaffolding & diagnostics"
sub "riz init --list — 7 built-in project templates, embedded in the binary."
run "$BIN init --list"
sub "riz doctor — preflight: validates config, checks runtimes on PATH, probes the port."
run "$BIN --config $CFG doctor 2>&1 | tail -16"
pause

# ── Final telemetry sweep ───────────────────────────────────────────
banner "Final telemetry — /_riz/health after the run"
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

printf '\n%s%s✓ demo complete.%s  Every capability exercised live.  Server log: %s\n' "$BOLD" "$GREEN" "$RESET" "$LOG"
