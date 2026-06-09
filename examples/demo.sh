#!/bin/sh
# ╔══════════════════════════════════════════════════════════════════════╗
# ║  riz — complete narrated walkthrough of EVERY capability.             ║
# ╚══════════════════════════════════════════════════════════════════════╝
#
# Boots one riz instance from examples/riz.all.toml and demonstrates, live:
#   • System surface          /ready, /_riz/registry, /_riz/health, /_riz/metrics
#   • MCP server (2025-11-25)  raw JSON-RPC wire test + built-in inspector
#   • LLM gateway (live)       OpenAI-compatible /_riz/v1 → a REAL local model
#                              via Ollama (llama3.2:1b), plus the mock provider
#   • All FIVE runtimes        Bun, Node.js, Python, Rust, WASM — one envelope
#   • Capability-sandboxed WASM  a wasm32-wasip1 handler run under wasmtime
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
#   - the WASM echo handler    (this script builds it; needs the wasm target:
#                               rustup target add wasm32-wasip1 — soft-skipped)
#   - bun, node, python3 on PATH   (TS / JS / Python handlers)
#   - ollama     (live local-model gateway demo; soft-skipped if missing —
#                 install: brew install ollama)
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

# ── Readable output primitives ──────────────────────────────────────
# The whole point: each capability prints a short narration, the request as
# METHOD /path, and a ONE-LINE result. No exploded JSON, no raw curl flags.
banner() { printf '\n%s%s━━━ %s ━━━%s\n' "$BOLD" "$AMBER" "$1" "$RESET"; }
sub()    { printf '%s%s%s\n' "$DIM" "$1" "$RESET"; }                 # narration
req()    { printf '  %s%s%s\n' "$CYAN" "$1" "$RESET"; }              # the request line
out()    { printf '      %s\n' "$1"; }                               # its result, indented
kv()     { printf '  %s%-22s%s %s\n' "$CYAN" "$1" "$RESET" "$2"; }   # aligned label → value
ok()     { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$1"; }
warn()   { printf '  %s!%s %s\n' "$AMBER" "$RESET" "$1"; }
# jq helpers — extract a compact value; degrade to the (already-compact) body
# when jq is absent. riz emits single-line JSON, so no-jq output stays readable.
J()  { if [ "$HAS_JQ" = 1 ]; then jq -r "$1" 2>/dev/null; else cat; fi; }
JC() { if [ "$HAS_JQ" = 1 ]; then jq -c "$1" 2>/dev/null; else cat; fi; }
# indent any multi-line block by 6 spaces (for the few real tables we show)
indent() { sed 's/^/      /'; }
pause()  {
  [ "$PAUSE" = "1" ] || return 0
  printf '%s[press ENTER to continue]%s ' "$DIM" "$RESET"
  read -r _
}

# ── Build the WASM handler ──────────────────────────────────────────
# echo-wasm is an independent crate built for wasm32-wasip1 (not part of the
# host workspace). Build it here so the wasm runtime has a module to run.
WASM_DIR="$ROOT/examples/lambdas/echo-wasm"
WASM_OUT="$WASM_DIR/target/wasm32-wasip1/release/echo-wasm.wasm"
HAS_WASM=0
banner "Building the WASM handler (wasm32-wasip1)"
if [ -f "$WASM_OUT" ]; then
  HAS_WASM=1
  ok "already built: ${WASM_OUT#"$ROOT/"}"
elif command -v rustup >/dev/null 2>&1 && rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1; then
  sub "cargo build --release --target wasm32-wasip1  (in examples/lambdas/echo-wasm)"
  if (cd "$WASM_DIR" && cargo build --release --target wasm32-wasip1 >/tmp/riz-wasm-build.log 2>&1); then
    HAS_WASM=1; ok "built ${WASM_OUT#"$ROOT/"}"
  else
    warn "wasm build failed (see /tmp/riz-wasm-build.log) — /echo-wasm will be skipped"
  fi
else
  warn "wasm32-wasip1 target not installed — run: rustup target add wasm32-wasip1"
  warn "the /echo-wasm runtime will be skipped (every other runtime still runs)"
fi
# If the module is missing, drop the echo-wasm function from the config we boot
# so riz doctor/boot stays clean; otherwise use the full config as-is.
CFG_RUN="$CFG"
if [ "$HAS_WASM" = "0" ]; then
  CFG_RUN="$(mktemp)"   # riz reads any path as TOML; extension is irrelevant
  awk 'BEGIN{skip=0}
       /^\[function\.echo-wasm\]/{skip=1}
       /^\[function\.auth-allow\]/{skip=0}
       skip==0{print}' "$CFG" > "$CFG_RUN"
fi

# ── Boot ────────────────────────────────────────────────────────────
banner "Booting riz"
if [ "$HAS_WASM" = "1" ]; then
  sub "config: examples/riz.all.toml  (16 functions across Bun · Node.js · Python · Rust · WASM)"
else
  sub "config: examples/riz.all.toml  (15 functions across Bun · Node.js · Python · Rust)"
fi

lsof -ti :3000 2>/dev/null | xargs -r kill -TERM 2>/dev/null || true
sleep 1

LOG=/tmp/riz-demo.log
"$BIN" --log-level warn --config "$CFG_RUN" run >"$LOG" 2>&1 &
RIZ_PID=$!

# Single cleanup path for every exit. Vars are pre-initialised so `set -u`
# is happy if we exit before they're assigned. Handles: the riz server, an
# ollama server we started (not a pre-existing one), the hot-reload handler
# restore, and the temp no-wasm config.
OLLAMA_PID=""; PING_BAK=""; PING_TS=""
cleanup() {
  kill -TERM "$RIZ_PID" 2>/dev/null || true
  wait "$RIZ_PID" 2>/dev/null || true
  [ -n "$OLLAMA_PID" ] && kill -TERM "$OLLAMA_PID" 2>/dev/null || true
  if [ -n "$PING_BAK" ] && [ -f "$PING_BAK" ]; then
    cp "$PING_BAK" "$PING_TS" 2>/dev/null || true
    rm -f "$PING_BAK"
  fi
  [ "$CFG_RUN" != "$CFG" ] && rm -f "$CFG_RUN" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

printf '%swaiting for /ready %s' "$DIM" "$RESET"
for _ in $(seq 1 30); do
  if curl -fs "$BASE/ready" >/dev/null 2>&1; then
    printf '%s✓%s\n' "$GREEN" "$RESET"
    break
  fi
  printf '.'
  sleep 1
done
sleep 2  # let bun/node/python/rust/wasm workers finish spawning
sub "logs streaming to $LOG  (pid $RIZ_PID)"
pause

# ── Ollama warm-up (live LLM gateway) ───────────────────────────────
# Kick this off early so the model is loaded by the time we hit the gateway
# section. Entirely soft: if anything is missing the gateway demo uses mock.
OLLAMA_MODEL="llama3.2:1b"
OLLAMA_READY=0
# Resolve a WORKING ollama binary. The Homebrew *formula* (brew install ollama)
# has historically shipped without its `llama-server` inference runner, so
# `ollama serve` starts but every inference 500s. The official prebuilt app
# (brew install --cask ollama-app → /Applications/Ollama.app) bundles the
# runner, so prefer it when present; otherwise fall back to ollama on PATH
# (correct on Linux via the official install).
OLLAMA_BIN=""
if [ -x "/Applications/Ollama.app/Contents/Resources/ollama" ]; then
  OLLAMA_BIN="/Applications/Ollama.app/Contents/Resources/ollama"
elif command -v ollama >/dev/null 2>&1; then
  OLLAMA_BIN="$(command -v ollama)"
fi
if [ -n "$OLLAMA_BIN" ]; then
  banner "Warming up Ollama (live local model: $OLLAMA_MODEL)"
  sub "ollama binary: $OLLAMA_BIN"
  # Start the server if nothing is listening on :11434.
  if ! curl -fs http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
    sub "starting 'ollama serve' (background)…"
    "$OLLAMA_BIN" serve >/tmp/riz-ollama.log 2>&1 &
    OLLAMA_PID=$!
    for _ in $(seq 1 30); do
      curl -fs http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break
      sleep 1
    done
  else
    ok "ollama already serving on :11434"
  fi
  if curl -fs http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
    # Ensure the model is present (pull is idempotent; first run downloads ~1.3GB).
    if "$OLLAMA_BIN" list 2>/dev/null | grep -q "$OLLAMA_MODEL"; then
      ok "model $OLLAMA_MODEL present"
    else
      sub "pulling $OLLAMA_MODEL (first run only)…"
      "$OLLAMA_BIN" pull "$OLLAMA_MODEL" >/tmp/riz-ollama-pull.log 2>&1 \
        && ok "pulled $OLLAMA_MODEL" || warn "pull failed (see /tmp/riz-ollama-pull.log)"
    fi
    # Verify a REAL inference works (the formula-without-runner case 500s here).
    if "$OLLAMA_BIN" list 2>/dev/null | grep -q "$OLLAMA_MODEL" && \
       curl -fs http://127.0.0.1:11434/v1/chat/completions \
         -H 'content-type: application/json' \
         -d "{\"model\":\"$OLLAMA_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"stream\":false}" \
         >/dev/null 2>&1; then
      OLLAMA_READY=1
      ok "live inference verified — gateway will route ollama/$OLLAMA_MODEL to a real model"
    else
      warn "ollama is up but inference failed (often a brew *formula* missing its llama-server"
      warn "runner — install the prebuilt app: brew install --cask ollama-app). Using mock."
    fi
  else
    warn "ollama did not come up — gateway demo will use the mock provider"
  fi
  # An ollama server we started is torn down by cleanup() on exit; a
  # pre-existing one (OLLAMA_PID empty) is left running.
  pause
else
  warn "ollama not found — live-model gateway demo falls back to mock"
  warn "(install: brew install --cask ollama-app — bundles the llama-server runner)"
fi

# Column-format a TAB-separated stream into an aligned table (jq present);
# otherwise pass through. Used for the registry + final telemetry tables.
table() { if [ "$HAS_JQ" = 1 ] && command -v column >/dev/null 2>&1; then column -t -s "$(printf '\t')"; else cat; fi; }

# ── System surface ──────────────────────────────────────────────────
banner "System surface — exposed by every riz instance, zero config"

req "GET /ready"
out "HTTP $(curl -s -o /dev/null -w '%{http_code}' "$BASE/ready")  $(curl -s "$BASE/ready")"

sub "/_riz/registry — every function (system + user), one row each:"
curl -s "$BASE/_riz/registry" \
  | J '.functions[] | "\(.runtime // "system")\t\(.name)\t\(.routes | join(", "))"' \
  | table | indent

H=$(curl -s "$BASE/_riz/health")
req "GET /_riz/health"
out "$(printf '%s\n' "$H" | J '[.functions[]|select(.healthy)]|length')/$(printf '%s\n' "$H" | J '.functions|length') functions healthy · uptime $(printf '%s\n' "$H" | J '.uptime_secs')s · riz $(printf '%s\n' "$H" | J '.version')"

req "GET /_riz/metrics"
out "Prometheus exposition · $(curl -s "$BASE/_riz/metrics" | grep -c '^riz_invocations_total') invocation counters + error & latency series"
pause

# ── MCP ─────────────────────────────────────────────────────────────
banner "MCP server — /_riz/mcp speaks JSON-RPC 2.0 (spec 2025-11-25)"
sub "Raw JSON-RPC — exactly what Claude Code / Cursor / the MCP Inspector send."
mcp() { curl -s -X POST -H 'content-type: application/json' -d "$1" "$BASE/_riz/mcp"; }

I=$(mcp '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"demo","version":"0.1.0"}}}')
req "initialize"
out "protocol $(printf '%s\n' "$I" | J '.result.protocolVersion') · server $(printf '%s\n' "$I" | J '.result.serverInfo.name') $(printf '%s\n' "$I" | J '.result.serverInfo.version')"

T=$(mcp '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')
req "tools/list"
out "$(printf '%s\n' "$T" | J '.result.tools|length') tools, one per function (auto-exposed):"
printf '%s\n' "$T" | J '.result.tools[].name' | paste -sd ' ' - | fold -s -w 64 | indent

C=$(mcp '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ping","arguments":{}}}')
req "tools/call ping"
out "statusCode $(printf '%s\n' "$C" | J '.result.structuredContent.statusCode') · body $(printf '%s\n' "$C" | J '.result.structuredContent.body')"

sub "Built-in self-validating client (no external deps):"
"$BIN" mcp inspect 2>&1 | sed -n '1,4p' | indent
pause

# ── LLM gateway ─────────────────────────────────────────────────────
banner "LLM gateway — OpenAI-compatible API at /_riz/v1"
sub "Point any OpenAI client here. Two providers wired: 'mock' (deterministic)"
sub "and 'ollama' (a REAL local model, no key). Route by model prefix + fallback."
chat() { curl -s -X POST -H 'content-type: application/json' -d "$1" "$BASE/_riz/v1/chat/completions"; }

req "GET /v1/models"
out "providers: $(curl -s "$BASE/_riz/v1/models" | J '[.data[].id]|join(", ")')"

R=$(chat '{"model":"mock","messages":[{"role":"user","content":"hello riz"}],"stream":false}')
req "POST /v1/chat/completions   (model = mock)"
out "→ $(printf '%s\n' "$R" | J '.choices[0].message.content')   [$(printf '%s\n' "$R" | J '.usage.total_tokens') tok]"

if [ "$OLLAMA_READY" = "1" ]; then
  R=$(chat "{\"model\":\"ollama/$OLLAMA_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"In one short sentence, what is AWS Lambda?\"}],\"stream\":false}")
  req "POST /v1/chat/completions   (model = ollama/$OLLAMA_MODEL — REAL model)"
  printf '%s\n' "$R" | J '.choices[0].message.content' | fold -s -w 64 | sed 's/^/      → /;2,$s/^      → /        /'
  ok "live local inference · $(printf '%s\n' "$R" | J '.usage.prompt_tokens')→$(printf '%s\n' "$R" | J '.usage.completion_tokens') tokens attributed"
else
  warn "ollama not ready — gateway falls back to the mock provider"
fi

SC=$(curl -sN -X POST -H 'content-type: application/json' \
  -d '{"model":"mock","messages":[{"role":"user","content":"stream me"}],"stream":true}' \
  "$BASE/_riz/v1/chat/completions" | grep -c '^data:')
req "POST /v1/chat/completions   (stream = true)"
out "$SC Server-Sent Events streamed (chat.completion.chunk … [DONE])"

E=$(curl -s -X POST -H 'content-type: application/json' -d '{"model":"mock","input":["the quick brown fox","lorem ipsum"]}' "$BASE/_riz/v1/embeddings")
req "POST /v1/embeddings"
out "$(printf '%s\n' "$E" | J '.data|length') vectors × $(printf '%s\n' "$E" | J '.data[0].embedding|length') dims"

req "GET /v1/usage   (AI-FinOps)"
out "$(curl -s "$BASE/_riz/v1/usage" | J '[.providers | to_entries[] | "\(.key): \(.value.requests) req, $\(.value.cost_usd)"] | join("   ·   ")')"
sub "Set [gateway] budget_usd to cap spend — over-budget requests return HTTP 412."

if python3 -c 'import openai' >/dev/null 2>&1; then
  req "OpenAI(base_url=\"$BASE/_riz/v1\").chat.completions.create(model=\"mock\", …)"
  RIZ_OPENAI_BASE="$BASE/_riz/v1" python3 - <<'PY' | indent
import os
from openai import OpenAI
c = OpenAI(base_url=os.environ["RIZ_OPENAI_BASE"], api_key="not-needed")
r = c.chat.completions.create(model="mock",
    messages=[{"role": "user", "content": "hi from the openai client"}], stream=False)
print("→", r.choices[0].message.content)
PY
else
  sub "(official openai python client speaks this exact wire; pip install openai to see it)"
fi
pause

# ── Five runtimes ───────────────────────────────────────────────────
banner "Five runtimes — one Lambda envelope, identical responses"
sub "The same GET hits five handlers in five languages; each returns the same shape."
erow() { # label  path  [extra-jq]
  r=$(curl -s "$BASE$2?name=alice")
  v="method $(printf '%s\n' "$r" | J '.method') · fn $(printf '%s\n' "$r" | J '.functionName')"
  [ -n "${3:-}" ] && v="$v · $(printf '%s\n' "$r" | J "$3")"
  printf '  %s%-22s%s %s\n' "$CYAN" "$1" "$RESET" "$v"
}
erow "echo-bun (Bun)"       /echo-bun
erow "echo-node (Node.js)"  /echo-node
erow "echo-python (Python)" /echo-python
erow "echo-rust (Rust)"     /echo-rust
if [ "$HAS_WASM" = "1" ]; then
  erow "echo-wasm (WASM)"   /echo-wasm  '"sandbox=" + .stageVariables.sandbox'
else
  warn "echo-wasm skipped (wasm module not built — see the build step above)"
fi
pause

# ── HTTP request shapes ─────────────────────────────────────────────
banner "HTTP request shapes"
req "GET /accounts/42?include=profile   (path param {id}=42 + query)"
out "$(curl -s "$BASE/accounts/42?include=profile" | JC '.')"

req "POST /events   (JSON body)"
out "$(curl -s -X POST -H 'content-type: application/json' -d '{"event":"login","user":"alice"}' "$BASE/events" | JC '.')"

sub "crud-accounts — one handler, five verbs:"
out "POST   /accounts → $(curl -s -X POST -H 'content-type: application/json' -d '{"name":"alice","plan":"pro"}' "$BASE/accounts" | JC '{id,name,plan}')"
out "GET    /crud/1   → HTTP $(curl -s -o /dev/null -w '%{http_code}' "$BASE/crud/1")"
out "DELETE /crud/1   → HTTP $(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/crud/1")"
pause

# ── Response caching ────────────────────────────────────────────────
banner "Response caching — GET /accounts/{id} cached 30s, then invalidated"
sub "Handler stamps a 'ts'. A cache HIT replays it; invalidate evicts → fresh ts."
TS1=$(curl -s "$BASE/accounts/7" | J '.ts'); TS2=$(curl -s "$BASE/accounts/7" | J '.ts')
out "ts #1 $TS1   ·   ts #2 $TS2"
if [ "$TS1" = "$TS2" ]; then ok "identical → served from cache"; else warn "differ"; fi
curl -s -o /dev/null -X POST -H 'content-type: application/json' -d '{"prefix":"GET:/accounts/"}' "$BASE/cache/invalidate"
TS3=$(curl -s "$BASE/accounts/7" | J '.ts')
out "ts #3 $TS3   (after POST /cache/invalidate)"
if [ "$TS3" != "$TS1" ]; then ok "changed → cache evicted, handler re-ran"; else warn "unchanged"; fi
pause

# ── CORS ────────────────────────────────────────────────────────────
banner "CORS — global policy (allow_origins = app.example.com)"
req "OPTIONS /ping   (preflight from allowed origin)"
out "HTTP $(curl -s -o /dev/null -w '%{http_code}' -X OPTIONS -H 'Origin: https://app.example.com' -H 'Access-Control-Request-Method: GET' "$BASE/ping")  (+ Access-Control-* headers)"
req "GET /ping   (with Origin header)"
out "$(curl -s -i -H 'Origin: https://app.example.com' "$BASE/ping" | grep -i '^access-control-allow-origin' | tr -d '\r')"
pause

# ── Stage variables ─────────────────────────────────────────────────
banner "Stage variables — per-function config surfaced on the event"
req "GET /echo-bun"
out "stageVariables = $(curl -s "$BASE/echo-bun" | JC '.stageVariables')"
pause

# ── On-box safety ───────────────────────────────────────────────────
banner "On-box safety — per-function caps + an always-on profile"
sub "echo-python opts into caps; an always-on profile wraps EVERY handler:"
sed -n '/\[function.echo-python\]/,/routes\]\]/p' "$CFG" | grep -E '^(cpu_time_secs|allowed_paths)' | indent
ok "cpu_time_secs → RLIMIT_CPU · allowed_paths → Landlock (Linux) · memory_mb → RLIMIT_AS"
ok "always-on (every child): RLIMIT_CORE=0, fd/file-size caps, PDEATHSIG, NO_NEW_PRIVS"
pause

# ── Authorizers ─────────────────────────────────────────────────────
banner "Lambda authorizers (REQUEST type)"
req "POST /protected   (gated by auth-allow)"
out "HTTP $(curl -s -o /dev/null -w '%{http_code}' -X POST -d '{"event":"hello"}' "$BASE/protected")   → allowed"
req "GET /forbidden    (gated by auth-deny)"
out "HTTP $(curl -s -o /dev/null -w '%{http_code}' "$BASE/forbidden")   → denied, handler never ran"
sub "JWT authorizers (RS256/ES256 vs your IdP's JWKS — Auth0/Cognito/Okta) ship too;"
sub "see examples/riz.jwt.toml, proven in tests/wave_3_acceptance.rs."
pause

# ── WebSocket ───────────────────────────────────────────────────────
banner "WebSocket — \$connect / \$default / \$disconnect across 3 runtimes"
if command -v websocat >/dev/null 2>&1; then
  ws() {
    reply=$(printf '%s\n' "$2" | timeout 3 websocat "ws://127.0.0.1:3000$1" 2>/dev/null | head -1 || true)
    printf '  %s%-14s%s send %-10s → %s%s%s\n' "$CYAN" "$1" "$RESET" "$2" "$GREEN" "$reply" "$RESET"
  }
  ws /chat        hello-bun
  ws /chat-python hello-py
  ws /chat-rust   hello-rs
else
  warn "websocat not installed — skipping WS round-trips (brew install websocat)"
fi
pause

# ── Handler hot-reload ──────────────────────────────────────────────
banner "Handler hot-reload — edit a handler, next request runs new code"
PING_TS="$ROOT/examples/lambdas/ping/index.ts"
PING_BAK="$(mktemp)"; cp "$PING_TS" "$PING_BAK"
# cleanup() (set at boot) restores PING_TS from PING_BAK on any exit.
req "GET /ping   (before)"
out "$(curl -s "$BASE/ping" | JC '.')"
sub "editing ping/index.ts:  status \"ok\" → \"hot-reloaded\" …"
sed -i.swp 's/status: "ok"/status: "hot-reloaded"/' "$PING_TS" && rm -f "$PING_TS.swp"
printf '  %swaiting for hot-swap %s' "$DIM" "$RESET"
for _ in $(seq 1 20); do
  if curl -s "$BASE/ping" | grep -q 'hot-reloaded'; then printf '%s✓%s\n' "$GREEN" "$RESET"; break; fi
  printf '.'; sleep 0.5
done
req "GET /ping   (after — same pid $RIZ_PID, no restart)"
out "$(curl -s "$BASE/ping" | JC '.')"
cp "$PING_BAK" "$PING_TS"; rm -f "$PING_BAK"
ok "handler restored to original"
pause

# ── Scaffolding & diagnostics ───────────────────────────────────────
banner "Scaffolding & diagnostics"
sub "riz init — built-in project templates, embedded in the binary:"
"$BIN" init --list | grep -E 'TEMPLATE|-http|-websocket' | indent
sub "riz doctor — preflight (config, runtimes on PATH, port):"
out "$("$BIN" --config "$CFG" doctor 2>&1 | tail -1)"
pause

# ── Final telemetry ─────────────────────────────────────────────────
banner "Final telemetry — invocations after the run"
H=$(curl -s "$BASE/_riz/health")
printf '%s\n' "$H" | J '.functions[] | "\(.name)\t\(.invocations) calls\t\(if .healthy then "healthy" else "DOWN" end)"' | table | indent
ok "uptime $(printf '%s\n' "$H" | J '.uptime_secs')s · $(printf '%s\n' "$H" | J '.functions|length') functions registered"

printf '\n%s%s✓ demo complete — every capability shown live.%s  Server log: %s\n' "$BOLD" "$GREEN" "$RESET" "$LOG"
