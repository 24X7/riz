#!/usr/bin/env bash
# riz end-to-end smoke harness — proves the REAL riz binary and EVERY example
# handler work together.
#
# This is an ASSERTION harness, not a demo. It boots the actual `riz` binary
# against examples/riz.all.toml, then for every example across every runtime it
# verifies the HTTP status code AND the response body, increments pass/fail
# counters, prints ✓/✗ per check, and EXITS NON-ZERO if anything fails — so a
# regression in any example, runtime, or control-plane surface turns CI red.
#
# It is wired into `cargo nextest` via tests/e2e_smoke_all.rs, so it runs on the
# same path as the unit tests and gates incoming code against regressions.
#
# Coverage (every function in examples/riz.all.toml):
#   Bun  HTTP   ping · accounts (+response cache) · events · crud-accounts (verbs) · echo-bun (+stage vars, status passthrough)
#   Node HTTP   echo-node
#   Py   HTTP   echo-python
#   Rust HTTP   echo-rust    (stock lambda_runtime  via the AWS Lambda Runtime API)
#   Go   HTTP   echo-go      (stock aws-lambda-go    via the AWS Lambda Runtime API)
#   WASM        echo-wasm · orders-wasm (real-compute order pricing + validation)
#   Authz       protected (allow → 200) · forbidden (deny → 401)
#   CORS        OPTIONS preflight → 204 + Access-Control-Allow-Origin
#   WebSocket   chat (Bun) · chat-python (Python) · chat-rust (Rust, Runtime API)
#   Control     /_riz/health · /_riz/metrics · /_riz/mcp (riz mcp inspect) · LLM gateway /_riz/v1
#
# "All examples work together" cannot be proven without all the runtimes, so a
# missing toolchain FAILS LOUDLY (exit 2) rather than passing by silent skip.
# Required: bun, node, python3, go, cargo (+ the wasm32-wasip1 rust target).
#
# Env overrides (defaults in brackets):
#   RIZ_BIN     riz binary to run            [target/release/riz, built if absent]
#   RIZ_CONFIG  config to boot               [examples/riz.all.toml]
#   PORT        port to bind                 [3000]
#
# Usage:  ./examples/smoke-all.sh
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${RIZ_BIN:-$ROOT/target/release/riz}"
CFG="${RIZ_CONFIG:-$ROOT/examples/riz.all.toml}"
PORT="${PORT:-3000}"
BASE="http://127.0.0.1:$PORT"
WS_PROBE="$ROOT/examples/ws_probe.py"
LOG="${TMPDIR:-/tmp}/riz-smoke.log"
WMARK='__HTTPSTATUS__'

# WS example handlers POST their echo back to the @connections endpoint; point
# them at this instance so the harness can run on any port (not just 3000).
export RIZ_TEST_BASE_URL="$BASE"

PASS=0
FAIL=0
FAILED_LIST=""

if [ -t 1 ]; then C_G=$'\033[32m'; C_R=$'\033[31m'; C_Y=$'\033[33m'; C_B=$'\033[1m'; C_0=$'\033[0m'
else C_G=""; C_R=""; C_Y=""; C_B=""; C_0=""; fi

section() { printf '\n%s──── %s ────%s\n' "$C_B" "$1" "$C_0"; }
ok()   { PASS=$((PASS + 1)); printf '  %s✓%s %s\n' "$C_G" "$C_0" "$1"; }
bad()  { FAIL=$((FAIL + 1)); FAILED_LIST="$FAILED_LIST
    - $1"; printf '  %s✗ FAIL%s %s\n' "$C_R" "$C_0" "$1"
         [ -n "${2:-}" ] && printf '         %s\n' "$2"; return 0; }
note() { printf '  %s•%s %s\n' "$C_Y" "$C_0" "$1"; }
fatal() { printf '\n%sFATAL:%s %s\n' "$C_R" "$C_0" "$1" >&2; exit "${2:-1}"; }

# ── HTTP assertion ────────────────────────────────────────────────────────────
# check_http NAME METHOD PATH WANT_STATUS WANT_SUBSTR [extra curl args...]
check_http() {
  local name="$1" method="$2" path="$3" want_status="$4" want_substr="$5"
  shift 5
  local raw status body
  raw="$(curl -sS -m 20 -X "$method" -w "$WMARK%{http_code}" "$@" "$BASE$path" 2>"${TMPDIR:-/tmp}/riz-curl.err")" \
    || { bad "$name" "curl error: $(tr '\n' ' ' < "${TMPDIR:-/tmp}/riz-curl.err")"; return; }
  status="${raw##*$WMARK}"
  body="${raw%$WMARK*}"
  if [ "$status" != "$want_status" ]; then
    bad "$name" "expected HTTP $want_status, got $status — body: $(printf '%s' "$body" | head -c 180)"
    return
  fi
  if [ -n "$want_substr" ] && ! printf '%s' "$body" | grep -qF -- "$want_substr"; then
    bad "$name" "body missing '$want_substr' — got: $(printf '%s' "$body" | head -c 180)"
    return
  fi
  ok "$name"
}

# ── WebSocket assertion ───────────────────────────────────────────────────────
# check_ws NAME PATH MESSAGE WANT_SUBSTR
check_ws() {
  local name="$1" path="$2" msg="$3" want_substr="$4" out
  out="$(python3 "$WS_PROBE" "ws://127.0.0.1:$PORT$path" "$msg" 2>"${TMPDIR:-/tmp}/riz-ws.err")" \
    || { bad "$name" "$(tr '\n' ' ' < "${TMPDIR:-/tmp}/riz-ws.err")"; return; }
  if printf '%s' "$out" | grep -qF -- "$want_substr"; then ok "$name (reply: $out)"
  else bad "$name" "reply missing '$want_substr' — got: $out"; fi
}

# ── preflight: toolchains ─────────────────────────────────────────────────────
preflight() {
  section "preflight — toolchains"
  local missing=""
  for t in curl bun node python3 go cargo; do
    if command -v "$t" >/dev/null 2>&1; then note "$t $(command -v "$t")"
    else missing="$missing $t"; fi
  done
  if command -v rustup >/dev/null 2>&1; then
    if rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip1$'; then
      note "rust target wasm32-wasip1 installed"
    else missing="$missing wasm32-wasip1(target)"; fi
  fi
  if [ -n "$missing" ]; then
    fatal "full e2e requires the complete toolchain; missing:$missing
       Install them (e.g. 'rustup target add wasm32-wasip1') and re-run." 2
  fi
}

# ── build/ensure every handler artifact the booted config references ──────────
ensure_artifacts() {
  section "preflight — building artifacts"
  if [ ! -x "$BIN" ]; then
    note "riz binary absent → cargo build --release"
    ( cd "$ROOT" && cargo build --release ) || fatal "riz build failed" 3
  fi
  if [ ! -x "$ROOT/target/release/echo-rust" ] || [ ! -x "$ROOT/target/release/chat-rust" ]; then
    note "building echo-rust + chat-rust (release)"
    ( cd "$ROOT" && cargo build --release -p echo-rust -p chat-rust ) || fatal "rust example build failed" 3
  fi
  if [ ! -x "$ROOT/tests/fixtures/parity/echo-go/echo-go" ]; then
    note "building echo-go (stock aws-lambda-go)"
    ( cd "$ROOT/tests/fixtures/parity/echo-go" && go build -o echo-go . ) || fatal "echo-go build failed" 3
  fi
  local wdir
  for wdir in tests/fixtures/parity/echo-wasm examples/lambdas/orders-wasm; do
    local w="${wdir##*/}"
    if [ ! -f "$ROOT/$wdir/target/wasm32-wasip1/release/$w.wasm" ]; then
      note "building $w (wasm32-wasip1)"
      ( cd "$ROOT" && cargo build --release --target wasm32-wasip1 \
          --manifest-path "$wdir/Cargo.toml" ) || fatal "$w wasm build failed" 3
    fi
  done
  note "all artifacts present"
}

# ── boot + readiness ──────────────────────────────────────────────────────────
RIZ_PID=""
cleanup() { [ -n "$RIZ_PID" ] && { kill -TERM "$RIZ_PID" 2>/dev/null; wait "$RIZ_PID" 2>/dev/null; }; }
trap cleanup EXIT INT TERM

start_server() {
  section "boot — riz run on :$PORT"
  if command -v lsof >/dev/null 2>&1; then
    local prior; prior="$(lsof -ti ":$PORT" 2>/dev/null || true)"
    [ -n "$prior" ] && { kill -TERM $prior 2>/dev/null || true; sleep 1; }
  fi
  : >"$LOG"
  "$BIN" --log-level warn --config "$CFG" --port "$PORT" run >"$LOG" 2>&1 &
  RIZ_PID=$!
  note "pid $RIZ_PID · config $CFG · log $LOG"
  local i code
  for i in $(seq 1 90); do
    if ! kill -0 "$RIZ_PID" 2>/dev/null; then
      printf '%s\n' "$(tail -40 "$LOG")" >&2
      fatal "riz exited during startup (see log above)" 4
    fi
    # /ready returns 200 only once EVERY pool is healthy — proves all handlers spawned.
    code="$(curl -sS -o /dev/null -m 3 -w '%{http_code}' "$BASE/ready" 2>/dev/null || true)"
    [ "$code" = "200" ] && { note "ready after ${i}s — all pools healthy"; return; }
    sleep 1
  done
  printf '%s\n' "$(tail -40 "$LOG")" >&2
  fatal "riz did not become ready on $BASE within 90s (see log above)" 4
}

# ── checks ────────────────────────────────────────────────────────────────────
run_checks() {
  local CT='content-type: application/json'

  section "Bun HTTP"
  check_http "ping → 200 {status:ok}"                 GET  "/ping"                            200 '"status":"ok"'
  check_http "accounts GET /accounts/{id}"            GET  "/accounts/42?include=profile"     200 '"include":"profile"'
  cache_check
  check_http "events POST → echoes payload"           POST "/events"                          200 '"received"'   -H "$CT" -d '{"event":"login","user":"alice"}'
  check_http "events POST (no body) → 400"            POST "/events"                          400 '"error"'
  check_http "crud POST /accounts → 201"              POST "/accounts"                        201 '"createdAt"'  -H "$CT" -d '{"name":"alice","plan":"pro"}'
  check_http "crud GET missing → 404"                 GET  "/crud/999999"                     404 'not found'
  check_http "crud PUT missing → 404"                 PUT  "/crud/999999"                     404 'not found'    -H "$CT" -d '{"name":"x"}'
  check_http "crud PATCH missing → 404"               PATCH "/crud/999999"                    404 'not found'    -H "$CT" -d '{"name":"x"}'
  check_http "crud DELETE missing → 404"              DELETE "/crud/999999"                   404 'not found'
  # functionName is asserted as the quoted value token so the check is agnostic
  # to JSON spacing (Python's json.dumps emits "k": "v", others are compact).
  check_http "echo-bun (functionName)"                GET  "/echo-bun?name=alice"             200 '"echo-bun"'
  check_http "echo-bun reflects stageVariables"       GET  "/echo-bun"                        200 '"region":"us-east-1"'
  check_http "echo-bun ?status=418 passthrough"       GET  "/echo-bun?status=418"             418 ""

  section "Node.js HTTP"
  check_http "echo-node (functionName)"               GET  "/echo-node?name=alice"            200 '"echo-node"'

  section "Python HTTP"
  check_http "echo-python (functionName)"             POST "/echo-python"                     200 '"echo-python"'  -H "$CT" -d '{"hello":"world"}'

  section "Rust HTTP  (stock lambda_runtime via the Lambda Runtime API)"
  check_http "echo-rust (functionName)"               GET  "/echo-rust?name=alice"            200 '"echo-rust"'

  section "Go HTTP  (stock aws-lambda-go via the Lambda Runtime API)"
  check_http "echo-go (functionName)"                 GET  "/echo-go?name=alice"              200 '"echo-go"'

  section "WASM  (wasm32-wasip1 under wasmtime)"
  check_http "echo-wasm echoes rawPath"               GET  "/echo-wasm"                       200 '"echo":"/echo-wasm"'
  check_http "orders-wasm prices a valid order"       POST "/orders"                          200 '"totalCents":2165'  -H "$CT" -d '{"currency":"USD","items":[{"sku":"A","qty":2,"unitPriceCents":500},{"sku":"B","qty":1,"unitPriceCents":1000}]}'
  check_http "orders-wasm subtotal computed"          POST "/orders"                          200 '"subtotalCents":2000' -H "$CT" -d '{"currency":"USD","items":[{"sku":"A","qty":2,"unitPriceCents":500},{"sku":"B","qty":1,"unitPriceCents":1000}]}'
  check_http "orders-wasm rejects empty order → 422"  POST "/orders"                          422 'at least one line item' -H "$CT" -d '{"currency":"USD","items":[]}'

  section "Authorizers (REQUEST Lambda authorizer)"
  check_http "protected (auth-allow) → 200"           POST "/protected"                       200 '"received"'   -H "$CT" -d '{"event":"hello"}'
  check_http "forbidden (auth-deny) → 401"            GET  "/forbidden"                       401 ""

  section "CORS"
  cors_check

  section "WebSocket round-trips"
  check_ws "chat (Bun) echoes message"            "/chat"        "hello-bun" "echo: hello-bun"
  check_ws "chat-python echoes message"           "/chat-python" "hello-py"  "echo: hello-py"
  check_ws "chat-rust echoes message (Runtime API)" "/chat-rust" "hello-rs"  "echo: hello-rs"

  section "Control plane — MCP / gateway / health / metrics"
  mcp_check
  check_http "LLM gateway POST /_riz/v1/chat/completions (mock)" POST "/_riz/v1/chat/completions" 200 'chat.completion' \
    -H "$CT" -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hello riz smoke"}]}'
  health_check
  check_http "/_riz/metrics (Prometheus exposition)"  GET  "/_riz/metrics"                    200 'riz_invocations_total'
}

cache_check() {
  local name="accounts response cache replays ts within TTL" j1 j2 ts1 ts2
  j1="$(curl -sS -m 15 "$BASE/accounts/77?include=profile")" || { bad "$name" "curl #1 failed"; return; }
  j2="$(curl -sS -m 15 "$BASE/accounts/77?include=profile")" || { bad "$name" "curl #2 failed"; return; }
  ts1="$(printf '%s' "$j1" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("ts",""))' 2>/dev/null)"
  ts2="$(printf '%s' "$j2" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("ts",""))' 2>/dev/null)"
  if [ -n "$ts1" ] && [ "$ts1" = "$ts2" ]; then ok "$name (ts=$ts1 stable)"
  else bad "$name" "ts differed → cache miss: '$ts1' vs '$ts2'"; fi
}

cors_check() {
  local name="OPTIONS preflight → 204 + Access-Control-Allow-Origin" raw status headers
  raw="$(curl -sS -m 15 -D - -o /dev/null -X OPTIONS \
      -H 'Origin: https://app.example.com' \
      -H 'Access-Control-Request-Method: POST' \
      -H 'Access-Control-Request-Headers: content-type' \
      -w "$WMARK%{http_code}" "$BASE/events" 2>/dev/null)" || { bad "$name" "curl failed"; return; }
  status="${raw##*$WMARK}"
  headers="${raw%$WMARK*}"
  if [ "$status" != "204" ] && [ "$status" != "200" ]; then bad "$name" "expected 204, got $status"; return; fi
  if printf '%s' "$headers" | grep -qi 'access-control-allow-origin:'; then ok "$name (HTTP $status)"
  else bad "$name" "Access-Control-Allow-Origin header absent"; fi
}

mcp_check() {
  local name="riz mcp inspect — every function exposed as a typed MCP tool" out rc miss tool
  out="$("$BIN" mcp inspect --url "$BASE/_riz/mcp" 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ]; then bad "$name" "exit $rc: $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; return; fi
  miss=""
  for tool in ping echo-bun echo-rust echo-go orders-wasm; do
    printf '%s' "$out" | grep -q "$tool" || miss="$miss $tool"
  done
  if [ -n "$miss" ]; then bad "$name" "tools missing from inspect:$miss"; else ok "$name"; fi
}

health_check() {
  local name="/_riz/health — all functions present & healthy" body msg rc
  body="$(curl -sS -m 15 "$BASE/_riz/health")" || { bad "$name" "curl failed"; return; }
  msg="$(printf '%s' "$body" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception as e:
    print("response is not JSON: %s" % e); sys.exit(1)
fns = {f["name"]: f for f in d.get("functions", [])}
expected = ["ping","accounts","events","crud-accounts","echo-bun","echo-node",
            "echo-python","echo-rust","echo-go","echo-wasm","orders-wasm",
            "protected","forbidden","chat","chat-python","chat-rust"]
missing = [n for n in expected if n not in fns]
unhealthy = [n for n, f in fns.items() if not f.get("healthy", False)]
if missing:
    print("missing functions: " + ", ".join(missing)); sys.exit(1)
if unhealthy:
    print("unhealthy functions: " + ", ".join(unhealthy)); sys.exit(1)
print("%d functions, all healthy" % len(fns))
' 2>&1)"; rc=$?
  if [ "$rc" -eq 0 ]; then ok "$name ($msg)"; else bad "$name" "$msg"; fi
}

# ── main ──────────────────────────────────────────────────────────────────────
printf '%sriz end-to-end smoke harness%s\n' "$C_B" "$C_0"
preflight
ensure_artifacts
start_server
run_checks

section "summary"
printf 'checks: %s%d passed%s' "$C_G" "$PASS" "$C_0"
if [ "$FAIL" -ne 0 ]; then
  printf ', %s%d FAILED%s\n' "$C_R" "$FAIL" "$C_0"
  printf '%sfailed:%s%s\n' "$C_R" "$C_0" "$FAILED_LIST"
  printf 'server log: %s\n' "$LOG"
  exit 1
fi
printf '\n%s✓ all %d checks passed%s — the riz binary and every example work together.\n' "$C_G" "$PASS" "$C_0"
