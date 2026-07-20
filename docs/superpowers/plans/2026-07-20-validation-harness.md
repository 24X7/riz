# Validation Harness (perf-regression + chaos + validate flow) Implementation Plan

> Status: in-progress. Standalone hardening work; lands before PR8 of the
> 2026-07-19 shape-purity spec so each new capability plugs into a proven
> fault-tolerance + perf flow.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** A solid, repeatable E2E flow — one command — that validates riz actually works, measures performance against a baseline, and survives deliberate chaos. Everything scoped to the built `riz` binary (`CARGO_BIN_EXE_riz`), isolated like `e2e_smoke_all`.

**Architecture:** Two net-new isolated nextest binaries (`perf_regression`, `chaos`) + one orchestration script (`scripts/validate.sh`) that chains the existing E2E (`e2e_smoke_all`, `template_smoke_all`) with the new ones. A small `tests/fixtures/chaos-handler` (bun) with `?sleep`/`?crash`/`?status` knobs drives fault injection. Reuses `tests/pg_wire_mock` for broker chaos.

## What already exists (build on, don't duplicate)
- `e2e_smoke_all` → `examples/smoke-all.sh`: 48 checks, all six runtimes + WS + authorizers + CORS + cache + MCP/gateway/health/metrics, on the **release** binary + full fleet. (install/fleet-E2E: covered)
- `template_smoke_all`: `riz new`→build→boot→roundtrip for all six templates. (scaffold-install: covered)
- `perf_http_floor`: conservative always-on throughput floor (150 req/s) — the catastrophe tripwire. Kept as-is.
- `runtime_parity_*`, `wasm_broker_pg`, `broker_limits`, `broker_secrets`, `broker_http`.

## Decisions (from owner)
- **Perf:** baseline band + trend. Always assert machine-portable relative invariants + the conservative floor; record absolute run metrics for trend; the 0.6×-baseline band gate is OPT-IN (`RIZ_PERF_GATE=1` + committed `tests/perf_baseline.json`) so noisy CI never flakes but a quiet machine can hard-gate.
- **Chaos:** full chaos as its own gated CI step (isolated binary, flake contained).
- **Install:** scaffold + fleet E2E (already covered; validate.sh chains them).

## Tasks
1. **chaos-handler fixture** (`tests/fixtures/chaos-handler/index.ts`, bun): honors `?status=NNN`, `?sleep=ms` (busy/await delay), `?crash=1` (`process.exit(1)` after responding-or-not to simulate a worker death). Plus a riz.toml the chaos/perf harnesses write on the fly.
2. **`tests/perf_regression.rs`** (isolated): boot built riz; measure (a) bun HTTP throughput + p50/p99 over a warm window, (b) per-runtime single-request p50 (skip missing toolchains), (c) capability round-trip p50/p99 via a wasm+pg grant against `pg_wire_mock` (guards the PR5 UDS-hop claim). Assert: floor RPS, relative invariants (p99 ≤ K·p50; capability p50 within N× of HTTP p50) — machine-portable. Write `target/perf-latest.json` (trend). If `RIZ_PERF_GATE=1` + baseline present, also assert each metric ≥ 0.6× baseline. Env `RIZ_PERF_UPDATE_BASELINE=1` writes the baseline.
3. **`tests/chaos.rs`** (isolated): boot built riz with the chaos-handler (concurrency ≥ 4) + a wasm+pg function on the mock. Tests: (a) **saturation → reject-not-queue** (flood slow reqs; a concurrent fast req still returns quickly; some get 429/503, none hang past deadline); (b) **worker respawn** (pgrep a child worker, SIGKILL it, assert serving recovers to 200); (c) **circuit breaker** (`?crash=1` repeatedly → 503 after the breaker trips); (d) **broker self-heal** (kill the pg mock mid-call → `backend`/`timeout` envelope; restart mock → next call succeeds); (e) **malformed input → no crash-loop** (garbage body → clean 4xx/5xx, server survives); (f) **SIGTERM graceful drain** (start a slow req, SIGTERM, assert it drains + process exits 0); (g) **no orphans** (after shutdown, pgrep finds no leaked riz/worker children). Toolchain-missing legs skip.
4. **`scripts/validate.sh`**: one command → build release riz, then run (in order, fail-fast, ✓/✗ report) e2e_smoke_all · template_smoke_all · perf_regression · chaos. This is the "solid flow that validates E2E" deliverable. Doc it in CONTRIBUTING.
5. **CI + docs**: add `chaos` + `perf_regression` as isolated CI steps (lockstep with the default nextest filter exclusion, mirroring e2e_smoke_all); update CLAUDE.md test-command list + CONTRIBUTING; CHANGELOG entry.
6. **Gates + ship**: full suite ×the smokes + the two new binaries; PR "test(harness): perf-regression + chaos E2E validation flow"; merge --admin.

## Self-Review
Honors all three owner decisions; builds on existing E2E rather than duplicating; scoped to the built binary + pgrep (no new pid endpoint); baseline gate is opt-in so CI stays green; chaos isolated so flake is contained; validate.sh is the single-command flow requested. Names: perf_regression, chaos, chaos-handler, validate.sh, RIZ_PERF_GATE, RIZ_PERF_UPDATE_BASELINE.
