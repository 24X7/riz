#!/bin/sh
# Reproducible riz benchmark.
# Spins up release riz with concurrency=20, runs wrk for 30s, prints the
# latency distribution. Tears the server down on exit.
#
# Requires: wrk on PATH. Install: `brew install wrk` (macOS) or build from
# https://github.com/wg/wrk
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/riz"
CFG="$ROOT/benches/bench-config.toml"

if [ ! -x "$BIN" ]; then
  echo "release binary missing — build first: cargo build --release" >&2
  exit 1
fi
if ! command -v wrk >/dev/null 2>&1; then
  echo "wrk not on PATH — install with: brew install wrk" >&2
  exit 1
fi

echo "starting riz (release, concurrency=20)..."
"$BIN" --no-tui --log-level warn --config "$CFG" run >/tmp/riz-bench.log 2>&1 &
RIZ_PID=$!
trap 'kill $RIZ_PID 2>/dev/null || true; wait $RIZ_PID 2>/dev/null || true' EXIT INT TERM

# Wait for /ready
for _ in $(seq 1 30); do
  if curl -fs http://127.0.0.1:3000/ready >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

# Let all 20 workers spawn
sleep 3

echo
echo "=== wrk: 30s, 20 connections (matches pool concurrency), 4 threads ==="
wrk -t4 -c20 -d30s --latency http://127.0.0.1:3000/ping
