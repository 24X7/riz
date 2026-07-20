# Shape-purity Tranche 5 (PR6 — env scrubbing) Implementation Plan

> Status: completed — PR6. Tranche of the 2026-07-19 spec
> `2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Children no longer inherit the daemon's full environment. Every worker is spawned with `env_clear()` + an explicit allowlist, so a resource's DSN (and any other daemon secret) exists in exactly one process — the daemon. The function's own `[function.X.env]` remains the documented escape hatch.

**Architecture:** `spawn_process` (src/process/pool.rs) calls `cmd.env_clear()` immediately after `runtime.spawn_command(cfg)` (the adapters set program+argv only, never env, so clearing is safe), then copies an allowlisted base set from the daemon env, then the existing explicit `.env()` calls (broker env, `[function.X.env]`, AWS_LAMBDA_* for RuntimeApi) proceed unchanged — env_clear precedes them all, so Rust/Go RuntimeApi vars survive. Proven by the all-six-runtime e2e smoke (unchanged config still boots every runtime) plus a new secrets canary test.

## Global Constraints
- Same gates; `--squash --admin` merge. src/ under Power of 10.
- The allowlist is conservative and greppable; a runtime that needs another var gets it added with the e2e smoke as the proof, never a blanket passthrough.
- DSN env vars (`[resources.pg.*] dsn_env`) MUST NOT be in the allowlist — that is the whole point.

## Tasks
1. **env_clear + allowlist** (src/process/pool.rs): add `const SCRUBBED_ENV_ALLOWLIST: &[&str]` (PATH, HOME, TMPDIR, LANG, LC_ALL, LC_CTYPE, TZ, TERM, SSL_CERT_FILE, SSL_CERT_DIR, the six proxy vars) + `apply_base_env(cmd)` copying present ones from `std::env`; call `cmd.env_clear()` then `apply_base_env` right after `spawn_command`, before any `.env()`. Comment the ordering contract. Unit test: allowlist excludes an arbitrary secret-shaped name.
2. **Secrets canary test** (tests/broker_secrets.rs + tests/fixtures/env-dump): a bun handler returning `process.env`; boot riz with `RIZ_SECRET_CANARY=<val>` in the daemon env and a `[function.env-dump.env] KEPT="yes"` escape-hatch var; assert the response env (a) omits `RIZ_SECRET_CANARY`, (b) omits a DSN-shaped daemon var, (c) includes `PATH` (allowlisted) and `KEPT` (escape hatch). Skips cleanly if bun is absent.
3. **Gates + ship**: full suite ×3 smokes (e2e_smoke_all is the all-six proof); docs note in CHANGELOG; PR "feat(security): scrub worker env — env_clear + allowlist, secrets live only in the daemon (PR6)"; merge `--admin`.

## Self-Review
Covers spec PR6: env_clear + per-runtime allowlist (one shared conservative list, extended per-runtime only if smoke demands), ordering (env_clear precedes .env), AWS_LAMBDA_* preserved, escape hatch documented + tested, all-six smoke + canary. Names consistent (SCRUBBED_ENV_ALLOWLIST, apply_base_env).
