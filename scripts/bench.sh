#!/usr/bin/env bash
# One-command riz throughput benchmark.
#
# Builds release, boots riz HEADLESS against a single Bun ping handler,
# waits for /ping to return 200, warms up briefly, runs wrk, and tears the
# server down on exit. Prints the honest req/s + p99 with a one-line
# methodology note.
#
# Usage:
#   ./scripts/bench.sh            # defaults: port 3000, 20s, 20 conns, 4 threads
#   ./scripts/bench.sh --tty      # wrap wrk in script(1) for terminal-accurate output
#   PORT=4000 DURATION=10s CONNECTIONS=20 THREADS=4 ./scripts/bench.sh
#
# Requires: cargo + wrk on PATH (`brew install wrk` on macOS).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/riz"
CFG="$ROOT/benches/bench-config.toml"

# Tunables (env overrides).
PORT="${PORT:-3000}"
DURATION="${DURATION:-20s}"
CONNECTIONS="${CONNECTIONS:-20}"
THREADS="${THREADS:-4}"

TTY=0
for arg in "$@"; do
  case "$arg" in
    --tty) TTY=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# ── Preflight: required tools ───────────────────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not on PATH. Install Rust: https://rustup.rs" >&2
  exit 1
fi
if ! command -v wrk >/dev/null 2>&1; then
  echo "error: wrk not on PATH. Install with: brew install wrk (macOS)" >&2
  echo "       or build from https://github.com/wg/wrk" >&2
  exit 1
fi

# ── Build release ───────────────────────────────────────────────────────────
echo "building release binary..."
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"

if [ ! -x "$BIN" ]; then
  echo "error: release binary missing after build: $BIN" >&2
  exit 1
fi

# ── Boot riz headless (NOT --dev) ───────────────────────────────────────────
PING_URL="http://127.0.0.1:${PORT}/ping"
echo "starting riz (release, headless) on port ${PORT}..."
"$BIN" --log-level warn --port "$PORT" --config "$CFG" run >/tmp/riz-bench.log 2>&1 &
RIZ_PID=$!

# Tear the server down no matter how we exit.
cleanup() {
  kill "$RIZ_PID" 2>/dev/null || true
  wait "$RIZ_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── Wait for /ping → 200 (bounded poll) ─────────────────────────────────────
ready=0
for _ in $(seq 1 60); do
  if ! kill -0 "$RIZ_PID" 2>/dev/null; then
    echo "error: riz exited during startup. Last log lines:" >&2
    tail -n 20 /tmp/riz-bench.log >&2 || true
    exit 1
  fi
  code="$(curl -s -o /dev/null -w '%{http_code}' "$PING_URL" 2>/dev/null || echo 000)"
  if [ "$code" = "200" ]; then
    ready=1
    break
  fi
  sleep 0.5
done
if [ "$ready" -ne 1 ]; then
  echo "error: /ping never returned 200 within ~30s. Last log lines:" >&2
  tail -n 20 /tmp/riz-bench.log >&2 || true
  exit 1
fi

# ── Warm up (prime the worker pool / JIT) ───────────────────────────────────
echo "warming up..."
wrk -t1 -c"$CONNECTIONS" -d3s "$PING_URL" >/dev/null 2>&1 || true

# ── Benchmark ───────────────────────────────────────────────────────────────
echo
echo "=== wrk: ${DURATION}, ${CONNECTIONS} connections, ${THREADS} threads → ${PING_URL} ==="
if [ "$TTY" -eq 1 ]; then
  # macOS script(1): `script -q /dev/null <cmd...>` — gives wrk a pty so its
  # progress/terminal formatting renders accurately.
  script -q /dev/null wrk -t"$THREADS" -c"$CONNECTIONS" -d"$DURATION" --latency "$PING_URL"
else
  wrk -t"$THREADS" -c"$CONNECTIONS" -d"$DURATION" --latency "$PING_URL"
fi

echo
echo "methodology: release riz, single Bun ping handler, localhost loopback, concurrency=${CONNECTIONS}. The 'Requests/sec' line is the honest throughput; p99 is the '99%' row under Latency Distribution."
