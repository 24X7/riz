#!/usr/bin/env bash
# riz end-to-end validation flow — one command that proves the built binary
# ACTUALLY works: the full example fleet, the scaffold/install journey,
# performance against a floor + trend, and survival under deliberate chaos.
#
# Each stage is an isolated nextest binary scoped to the real `riz` binary
# (CARGO_BIN_EXE_riz). Stages run in order; the script exits non-zero on the
# first failure so CI (or a human before a release) gets one clear verdict.
#
# Usage:
#   scripts/validate.sh            # run every stage
#   RIZ_PERF_GATE=1 scripts/validate.sh   # also hard-gate perf vs the baseline
#
# Stages, in order of cost:
#   1. e2e_smoke_all     — the release binary + every example, all six runtimes
#   2. template_smoke_all— riz new → build → run, all six templates
#   3. perf_regression   — HTTP + capability throughput/latency, floor + trend
#   4. chaos             — saturation, worker crash, breaker, broker loss, drain
set -uo pipefail

cd "$(dirname "$0")/.."

green() { printf '\033[32m%s\033[0m\n' "$1"; }
red()   { printf '\033[31m%s\033[0m\n' "$1"; }
bold()  { printf '\033[1m%s\033[0m\n' "$1"; }

fail=0
stage() {
  local name="$1"; shift
  bold "── $name ──"
  if "$@"; then
    green "✓ $name"
  else
    red "✗ $name FAILED"
    fail=1
    # Perf/chaos are independent signals; keep going to surface every failure.
  fi
  echo
}

bold "riz validate — building the release binary + wasm fixtures"
cargo build --release || { red "release build failed"; exit 3; }
# The capability perf/chaos legs need the broker-wasm guest; build best-effort.
if rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1; then
  cargo build --release --target wasm32-wasip1 \
    --manifest-path tests/fixtures/broker-wasm/Cargo.toml || true
fi
echo

stage "E2E fleet (all six runtimes + control plane)" \
  cargo nextest run --test e2e_smoke_all
stage "Scaffold/install journey (six templates)" \
  cargo nextest run --test template_smoke_all
stage "Performance (floor + trend)" \
  cargo nextest run --test perf_regression --no-capture
stage "Chaos (fault injection)" \
  cargo nextest run --test chaos --test-threads 2

if [ "$fail" -eq 0 ]; then
  green "════════ riz validated: every stage passed ════════"
  exit 0
else
  red "════════ riz validation FAILED — see stages above ════════"
  exit 1
fi
