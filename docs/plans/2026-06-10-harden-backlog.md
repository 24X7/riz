# riz — Battle-Test & Harden Backlog

> One ranked backlog for hardening riz toward GA, consolidating roadmap spillover from the
> 2026-06 claims-truth & AI-substrate work (Phases 2–5) plus harness/flake debt found along the way.
> Ranked by **value × risk** (high = do first). Each item names the proof that closes it.
>
> Last updated: 2026-06-10

## How to read this
- **Value**: GTM/credibility impact (does it unblock a claim, a sale, or trust?).
- **Risk**: blast radius / likelihood of regression if left undone or done wrong.
- **Proof**: the automated test or artifact that marks the item done ("hold the line").

---

## P0 — Trust & correctness floor (do first)

### 1. Perf-claim CI gating
- **Why**: the homepage's `91k req/s · p99<1ms` is honestly classed `benchmark` (registry `perf-throughput`), not CI-gated. A regression in the dispatch path could silently erode it.
- **Do**: add a CI job that runs `scripts/bench.sh` against a fixed profile and gates a **conservative floor** (e.g. ≥ 40k req/s, p99 < 3ms on the CI runner) — not the headline number, which is hardware-specific. Keep the headline as a reproducible recipe in `benches/README.md`.
- **Proof**: a `bench-floor` CI gate + a documented runner profile; registry `perf-throughput` upgraded from `benchmark` to gated.

### 2. Integration-harness leak/teardown hygiene
- **Why**: integration tests that spawn real subprocess pools and return before the handler finishes (e.g. `tests/http_boundary.rs::gateway_timeout_returns_504_for_routed_request`, now deterministic via a 15× timeout margin) are reported by nextest as **"leaky"** — the child is still alive at test end. A PASS today, but it muddies leak signal and can mask a real descriptor leak.
- **Do**: add a shared test helper that explicitly drains/aborts the `ProcessManager` pool + axum server on test exit (RAII guard). Apply to the spawn-and-timeout tests first.
- **Proof**: `cargo nextest run` reports **0 leaky**.

### 3. Telemetry graceful shutdown + export resilience
- **Why**: the `TelemetrySupervisor` is currently leaked for process lifetime (correct for an always-on server) and `shutdown()` is `#[allow(dead_code)]`. On SIGTERM we should **flush** buffered spans before exit, and the OTLP exporter should **retry with backoff** on transient collector errors rather than drop.
- **Do**: wire `TelemetrySupervisor::shutdown()` into the server's graceful-shutdown path (flush + join, bounded); add bounded retry/backoff to `observability::otel::export`; tune batch size/flush interval.
- **Proof**: `tests/telemetry_*` extended — a shutdown-flush test (no span loss on clean exit) + an export-retry test (transient 503 → retried, not dropped). Host-isolation guarantee must still hold.

---

## P1 — Close the AI-substrate roadmap (highest GTM value)

### 4. WASM resource broker — v1 (Postgres-wire)
- **Why**: the agent-substrate story is strongest when sandboxed WASM can safely reach a DB. Design is specced (`docs/superpowers/specs/2026-06-10-wasm-resource-broker-design.md`); v1 keystone is Postgres-wire (covers **Neon + Supabase** + any PG).
- **Do**: implement the host-side `broker_call` dispatcher with the full resiliency envelope (allow-list, per-call timeout, concurrency cap, payload cap, rate limit, audit) and a `pg_query` capability; `[function.x.capabilities]` config; one richer WASM example using it.
- **Proof**: `tests/wasm_broker_pg.rs` — a WASM guest runs a parameterized query against a test PG (or an in-process PG-wire mock) under a capability grant; deny-by-default verified; a stalled/oversized query is bounded, not host-affecting. Flip the `roadmap-*` broker ribbon to proven only when green.

### 5. Deeper AI examples — multi-step agent loop + token rollup across tool chains
- **Why**: Phase 4 shipped a single-shot Agent SDK demo; the differentiated story is **token attribution across a multi-hop tool/agent chain** (Phase 2 spans roll up, but we lack an example/test exercising depth).
- **Do**: an agent-loop example that calls several riz MCP tools in sequence; assert the request root span's token totals equal the sum across the chat-completion children. Add a RAG-style example.
- **Proof**: `tests/telemetry_token_spans.rs` extended to a 3+ hop chain; an `examples/agent-loop/` demo.

### 6. CloudWatch / X-Ray OTLP→segment mapping validation
- **Why**: the single OTLP path *claims* CloudWatch/X-Ray fan-out; X-Ray's segment model ≠ OTLP spans 1:1. We export OTLP/HTTP-JSON but haven't validated against a real ADOT collector / AWS OTLP endpoint.
- **Do**: validate the emitted spans through an ADOT collector to X-Ray; document the mapping + any attribute massaging; pin the GenAI semantic-convention version.
- **Proof**: a doc + an integration test against a collector container (gated/nightly).

---

## P2 — Resilience & breadth

### 7. Multi-provider gateway resilience
- Provider failover under partial outage, circuit-breaking, and the roadmap **semantic-similarity cache** (`roadmap-gateway-deepened`). **Proof**: failover + circuit-breaker tests; cache hit-rate test.

### 8. Auth — live-tenant nightly smoke + rotation/ES256 depth
- Phase 3 proves WorkOS/Clerk shapes via recorded-JWKS + minted tokens (deterministic). Add a **nightly** live-tenant smoke (real WorkOS/Clerk test tenants — see `docs/ACCOUNTS-TO-PROVISION.md`), JWKS-rotation-under-load coverage, and explicit ES256 cases. **Proof**: nightly job + `tests/auth_rotation.rs`.

### 9. Record & replay (roadmap pillar)
- `roadmap-record-replay`: capture request/response/timings/downstream calls; `riz replay --since 1h --function chat`; dataset export. **Proof**: a capture→replay→diff test.

### 10. Smarter MCP (roadmap pillar)
- `roadmap-smarter-mcp`: per-route typed tool schemas auto-derived from TS/Python/Rust types; MCP over WebSocket; OAuth 2.1; federation. **Proof**: schema-derivation tests; a WS-MCP streaming test.

---

## P3 — Repo & process hygiene

### 11. Cleanliness ruleset expansion
- Extend `tests/repo_cleanliness.rs` slop patterns + a doc-freshness policy (which dirs are authoritative vs archive). **Proof**: the guard grows; no new slop.

### 12. Eval harness + prompt versioning (roadmap pillar)
- `roadmap-eval-harness`: versioned prompts as config; `riz eval <fn>` ranks prompt×model×guard on quality/cost/latency. **Proof**: an eval-run test.

---

## Closed by the 2026-06 work (for reference)
- Single-path OTLP observability + token-aware GenAI spans + isolated telemetry process (Phase 2).
- WorkOS/Clerk JWT/JWKS validation + Clerk no-`aud` support (Phase 3).
- Claude Agent SDK demo + MCP tool-call substrate test (Phase 4).
- Real-compute WASM example (Phase 5).
- Claims registry + truth/trust/cleanliness/docs-runnable guards — every live claim mapped to a real test (Phase 1).
