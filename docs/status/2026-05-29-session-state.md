# Session state — 2026-05-29

Pick-up doc for the multi-session work after wave 9. Captures what's
shipped, what's in flight, what's queued (with priority), what's
deferred, and which `BUG-NN` tracker entries are actually closed
vs still open.

## Shipped this multi-session arc (newest first)

Commits since `0542c1c4` (the wave-8 fix that kicked off the run):

| Commit | What |
|---|---|
| `8d8d22a9` | docs(readme): 30-second start uses riz init + mentions handler hot-reload |
| `b3ceef05` | feat(tier-2): `riz init <template>` — scaffold a working project in one command |
| `f655d859` | feat(tier-2): handler-source hot reload — edit handler, request reflects |
| `279813a2` | docs(tier-1): sync README + landing page + llms.txt to Wave-10 + WS list |
| `fa0e7058` | feat(wave-10.E): opt-in `allowed_paths` via Linux Landlock (LSM, kernel 5.13+) |
| `6009f0c0` | feat(wave-10.D): opt-in `memory_mb` + `cpu_time_secs` per-function caps |
| `9f9d65f4` | feat(wave-10.C): RLIMIT_NOFILE=4096 + RLIMIT_FSIZE=100MiB + RLIMIT_NPROC=256 (Linux) |
| `353172c6` | feat(wave-10.B): `PR_SET_PDEATHSIG` + `PR_SET_NO_NEW_PRIVS` on Linux |
| `729f6767` | feat(wave-10.A): RLIMIT_CORE=0 on every spawned child |
| `f6d84006` | feat(ws): `GET /_riz/connections` list endpoint + complete @connections E2E |
| `fe0e0176` | fix(BUG-20): adapters pass non-HTTP handler returns through verbatim |
| `1f4c7565` | test(parity-M): WS @connections management API negative-path E2E |
| `d474b917` | test(parity-L): hot-reload of riz.toml end-to-end via HTTP |
| `99b77c9b` | test(parity-K): cache layer hit/miss middleware |
| `b2fa96db` | test(parity-J): REQUEST authorizer test — discovered BUG-20 (adapter drops non-HTTP payload) |
| `ca9d4abf` | test(parity-I): CORS preflight middleware — happy + denied origin |
| `460b8604` | test(parity-H): cross-runtime error status code pass-through parity |
| `3fa7d5b7` | test(parity-G): cross-runtime binary body (inbound base64) parity |
| `39a9f523` | test(parity-F): cross-runtime response headers + Set-Cookie parity |
| `7c4ac69d` | fix(test): bump 504 timeout test cold-start window from 800ms to 3000ms (parallel-suite flake) |
| `da38ce86` | test(parity-E): cross-runtime stage-vars + cookies + custom-headers parity |
| `2b838701` | test(parity-D): cross-runtime path-params + query-string parity (caught QueryMap footgun) |
| `9289d7cd` | test(parity-C): cross-runtime HTTP verb + body parity — Bun + Python + Rust |
| `a122f747` | test(parity-A): cross-runtime echo parity — Bun + Python + Rust |
| `3b4fe3c2` | test(bug-01): regression gate for parse-failure pipe-desync fix |

**Test suite:** 670 passed, 0 failed, 5 skipped (as of `8d8d22a9`).

## In flight RIGHT NOW

1. **Python + Rust WebSocket adapters exercised by example/test** — Bun has WS examples + e2e; Python and Rust have HTTP-only. The dispatch path supports any runtime; need to prove it with a chat-python and chat-rust example + an end-to-end test for each.
2. **BUG-15 verification + WS load test** — wave-7.3 already replaced the global `route_stats` write-lock with atomics + per-entry mutexes (see `src/state.rs:477-508::record_invocation`). Believed RESOLVED but unmarked. Plan: write a WS load test (N concurrent messages, assert all replies). If no contention, mark BUG-15 resolved.

## Deferred (decisions, not work)

| Item | Why deferred | Trigger to revisit |
|---|---|---|
| **TLS termination** | User chose: "smart termination with Let's encrypt outside the service" | Build Dockerfile + recipe doc instead |
| **Telemetry (X-Ray vs OTel)** | Real architectural decision — native AWS X-Ray header propagation OR OTel as universal OR both with config switch. Not a code question; a product decision | Dedicated brainstorm session before implementation |

## Queued by tier (ordered by impact)

### Tier 3 — Production packaging
- **Publish v0.1.0 release binaries** to GitHub Releases — unblocks `web/install` (currently 404)
- **Dockerfile** — minimal alpine/distroless image bundling riz + bun + python3
- **systemd unit** — `/etc/systemd/system/riz.service` example
- **Signed release artifacts** — needs your signing infra (Apple Developer ID, Windows code-signing cert, etc.)

### Tier 4 — Performance + observability (post-telemetry decision)
- **BUG-16**: access logs missing `request_id` + `source_ip` — search `src/server.rs::push_log` call sites
- **`criterion` benchmarks** committed for the dispatch hot path
- **Cold-start histogram** in `riz_metrics`
- **Telemetry implementation** (after architectural decision above)

### Tier 5 — DX polish (smaller wins)
- **More `riz init` templates** — currently typescript-http + python-http. Add: rust-http, typescript-websocket
- **Per-route MCP tool schemas** — currently every function is a generic envelope; should derive per-route input types from path/query params
- **MCP-native tools** — `riz.tail_logs`, `riz.replay_request`, `riz.list_routes`, `riz.scaffold`
- **TUI logs tab** — currently logs are filtered per-route only; dedicated tab would help
- **Handler-hot-reload ignore patterns** — currently watches recursively; node_modules / target / __pycache__ writes spam hot-swaps
- **Config DX** — JSON schema export for editor autocomplete, "did you mean" hints on misconfig

### Tier 6 — Out-of-scope by design (still relevant to document)
- Non-HTTP event sources (SQS / SNS / S3 / EventBridge / scheduled) — explicit non-goal
- Lambda Layers — explicit non-goal (vendor deps in handler dir)
- Lambda Extensions — explicit non-goal
- REST API v1 (`ApiGatewayProxyRequest`) — explicit non-goal (use v2)
- Custom domain mappings — reverse-proxy concern
- VPC endpoints / private APIs — AWS-account-scoped concept

## BUG tracker — resolution status (cross-check before declaring fixed)

Source of truth is `docs/production-bugs.md` — this is a sanity ledger.

| Bug | Status | Evidence |
|---|---|---|
| BUG-01 | ✅ RESOLVED `3b4fe3c2` | regression gate `tests/bug_01_parse_failure_respawns.rs` |
| BUG-02 | ✅ RESOLVED (pre-session) | liveness watcher in `src/process/liveness.rs` |
| BUG-03 | ✅ RESOLVED `279813a2` (annotation only — fix is older) | `PipeDropGuard` at `src/process/mod.rs:232-241` |
| BUG-04 | ✅ RESOLVED `279813a2` | bounded `mpsc::channel(10_000)` + drain task `src/main.rs:140,284` |
| BUG-05 | ✅ RESOLVED (pre-session) | `axum.serve(...).with_graceful_shutdown(...)` |
| BUG-06 | ✅ RESOLVED (pre-session) | `src/hotreload.rs` diff-aware swap |
| BUG-07 | 🚧 OPEN | `/deploy` warning-only when neither `RIZ_DEPLOY_KEY` nor `allowed_cidrs` set. Should default-deny in non-dev mode. |
| BUG-08 | ✅ RESOLVED `279813a2` | `percent_decode()` at `src/router.rs:124` |
| BUG-09 | ✅ RESOLVED (pre-session) | base64 path for non-UTF8 bodies in `src/server.rs:333` |
| BUG-10 | ✅ RESOLVED (pre-session) | 413 on >10 MB body at `src/server.rs:325` |
| BUG-11 | ✅ RESOLVED (pre-session) | (verify in tracker) |
| BUG-12 | ✅ RESOLVED (pre-session) | auth-aware cache bypass at `src/server.rs:227` |
| BUG-13 | 🚧 OPEN | deploy doesn't verify new process survives before declaring success |
| BUG-14 | ✅ RESOLVED (pre-session) | (verify in tracker) |
| BUG-15 | 🚧 BELIEVED RESOLVED, unverified | wave-7.3 killed `route_stats` write-lock; current `record_invocation` uses atomics + per-entry mutexes (`src/state.rs:477-508`). Plan: WS load test to confirm no remaining contention. |
| BUG-16 | 🚧 OPEN | access logs missing `request_id` + `source_ip` — straightforward fix at `src/server.rs::push_log` call sites |
| BUG-17 | 🚧 OPEN (verify) | check tracker |
| BUG-18 | 🚧 OPEN (verify) | check tracker — concurrent deploys race on `/tmp/riz-deploy/<lambda>` |
| BUG-19 | 🚧 OPEN | reject zip entries where `file.is_symlink()` — `src/deploy.rs` extraction loop |
| BUG-20 | ✅ RESOLVED `fe0e0176` | adapters now passthrough non-HTTP shapes; regression gates in `tests/middleware_request_authorizer.rs` |

**Top remaining tracker work:** BUG-07 (deploy default-deny) + BUG-13 (deploy health-verify) + BUG-19 (zip symlink) — all `/deploy`-path bugs. Could fit in one focused commit if we want to close them as a batch.

## Marketing surface — current vs aspirational

### What the landing page + README + llms.txt say TODAY (post `279813a2`)

Every claim has a `tests/landing_page_contract.rs` truth-slice entry pointing to a real test. Drift-prevention enforces.

### What's UNDERCLAIMED today (you could honestly add)

- "Edit your handler source, save, next request reflects" — handler hot-reload is shipped but not yet on the landing page (only README). **Truth-slice entry needed.**
- "`riz init <template>` — working project in seconds" — shipped but not on the landing page. **Truth-slice entry needed.**
- "REQUEST authorizer verified end-to-end with Bun (post-BUG-20 fix)" — could be strengthened from generic "Lambda authorizers" to call out the recent verification

### What you CANNOT yet say (don't put on the page)

- "TLS / Let's Encrypt built-in" — deferred by design
- "OpenTelemetry / X-Ray support" — telemetry decision pending
- "Production-grade enterprise: signed binaries + SBOM" — needs signing infra you haven't provisioned
- "Released v0.1.0 binaries" — install script still 404s

## Language / workload truth table (re-verify before claiming)

| Workload | Bun (TS/JS) | Python 3 | Rust | Other |
|---|---|---|---|---|
| AWS HTTP API v2 | ✅ parity-tested | ✅ parity-tested | ✅ parity-tested | ❌ |
| WebSocket + @connections | ✅ e2e tested | ⏳ adapter exists, **in-flight this slice** | ⏳ adapter exists, **in-flight this slice** | ❌ |
| Non-HTTP event sources | ❌ out of scope by design | ❌ | ❌ | ❌ |

## Pick-up procedure if session is lost

1. Read this doc + `docs/production-bugs.md` (cross-check BUG status)
2. `git log --oneline -30` (the table above can drift if commits happen after this was written)
3. `cargo nextest run` — confirm baseline green (expect ~670 pass, 5 skip)
4. Resume from "In flight RIGHT NOW" section
5. The deferred telemetry brainstorm should happen as a separate user-driven session — don't dive into x-ray/otel without that conversation

## Acceptance criteria for the "in flight" work

### Python WS adapter (slice WS-Python)
- `examples/lambdas/chat-python/main.py` — equivalent to `examples/lambdas/chat/index.ts`
- `tests/middleware_ws_python.rs` — WS connect, send message, receive echo via @connections POST. Skip gracefully if `python3` not on PATH.
- Acceptance: `cargo nextest run --test middleware_ws_python` returns 1 passed (or 1 skipped if no python3)

### Rust WS adapter (slice WS-Rust)
- `examples/lambdas/chat-rust/` — new crate using `riz-rust-runtime`
- `tests/middleware_ws_rust.rs` — same shape as Python test
- Acceptance: same as above, plus `cargo build --release -p chat-rust` succeeds

### BUG-15 verification (slice perf-1)
- `tests/perf_ws_load.rs` — 100 concurrent WS messages through one connection, assert all replies received within timeout
- Acceptance: test passes; if it does, mark BUG-15 resolved in `docs/production-bugs.md` with link to this test
- If it FAILS (contention bites), surface the real lock site and reopen BUG-15 with concrete repro

## What to NOT do until told

- **Don't pick a telemetry crate.** Don't write OTel or X-Ray code. That decision is upstream.
- **Don't publish release binaries.** Needs user-provisioned signing infra.
- **Don't refactor for "while we're here" cleanups.** Stay tightly scoped to the items above.
- **Don't add non-HTTP event sources.** Explicit non-goal.
