# Riz v1 Roadmap — Ranked, Shovel-Ready

**Status:** plan-of-record · written 2026-06-08 · supersedes ad-hoc roadmap on landing page
**Goal:** Take the v0.1 Lambda-runtime-with-MCP and ship v1 as the agent-native Lambda substrate. Most of the post-v0.1 features land in v1; the compound subsystems (embeddings, eval scoring, OAuth, federation) push to v2.

---

## Direction

Riz v0.1 is the Lambda runtime your agent can call. Riz v1 is the Lambda runtime your agent can **build against** — same wire contract, but with non-HTTP event sources, WASM-sandboxed guards + runtime, an LLM gateway, OpenTelemetry, two more native runtimes, and the foundations of record/replay. v2 ships the compound systems that need embeddings, scoring, or new auth flows.

**One rule:** every v1 item must be a coherent atomic shipment — one config block, one subsystem, one test file. Anything that requires two new subsystems to land usefully is pushed to v2.

---

## What's IN v1 (16 items, ordered)

Tier-1 (must-ship core, deliver first):

1. Event sources — SQS, SNS, S3, EventBridge
2. WASM standalone runtime (`runtime = "wasm"`)
3. WASM pre-invoke guards (`guard_in`)
4. WASM post-invoke guards (`guard_out`)
5. Node.js native runtime
6. Go native runtime
7. OpenTelemetry exporter

Tier-2 (strong adds, ship second wave):

8. LLM gateway — provider routing (Anthropic + OpenAI + Ollama first; Bedrock + Vertex follow)
9. LLM gateway — budget caps + cost telemetry
10. LLM gateway — OpenAI-compatible endpoint (`POST /_riz/v1/chat/completions`)
11. MCP over WebSocket transport
12. LLM gateway — streaming pass-through (depends on 11)
13. Record (capture) — every invocation persisted
14. Dataset export (depends on 13)
15. Per-route MCP tool schemas
16. Auto-derived MCP schemas from handler code (TS first, then Python, then Rust)

---

## What's OUT of v1 (deferred to v2 or later)

These are all good ideas — they just need too many parts to land safely inside v1.

| Item | Why deferred |
|---|---|
| Replay CLI (`riz replay`) | Needs Record (#13) + a dispatcher entry point for synthetic invocations + a diff renderer. Real work; do it once #13 is bedded down. |
| Agentic test loop | Needs Replay above + a regression-rank algorithm. |
| Semantic-similarity cache | Needs an embeddings provider + a similarity index. Whole new subsystem. |
| Prompt versioning | Needs a config schema design + filesystem layout for prompt branches. |
| Eval harness (`riz eval`) | Needs Prompt versioning + Gateway + a scoring rubric per-domain. The heaviest of all. |
| A/B win rate in `/_riz/health` | Needs Eval harness output. |
| OAuth 2.1 + RFC 8707 Resource Indicators | New auth subsystem; bearer-token covers the v1 buyer. |
| MCP federation | Needs a discovery protocol + multi-instance lifecycle. |
| Stateful agent memory (`/_riz/memory/{agent_id}`) | Not in scope per user: "outside that scope, should not be there but needed." Belongs in a separate "agent stateful layer" project. |
| Java / JVM runtime | Not in scope per user: "minus Java." |

---

## Ranked items — shovel-ready specs

Each entry has: **Why** · **Acceptance** · **Touches** · **Depends on** · **Effort** (S = 1–3d, M = 4–10d, L = 10d+).

### 1. Event sources — SQS, SNS, S3, EventBridge

**Why.** Lambda's biggest non-HTTP surface. Highest customer pull on the v1 list. Lambda contract already defines the event shapes via `aws_lambda_events`.
**Acceptance.**
- `riz.toml` accepts `[function.X.events.sqs]`, `.sns`, `.s3`, `.eventbridge` blocks
- Each adapter is a long-lived task in the runtime that polls / listens and invokes the function exactly as if API GW had called it
- Functions receive the canonical AWS event types (`SqsEvent`, `S3Event`, etc.) unchanged
- One end-to-end test per source (start adapter, push an event via aws-sdk against localstack or a faux endpoint, assert handler ran)
- `/_riz/registry` reports event sources alongside routes
**Touches.** `src/event_sources/{sqs,sns,s3,eventbridge}.rs`, `src/config.rs` (new types), `src/server.rs` (wire adapter tasks), `src/state.rs` (registry), `tests/event_sources_*.rs`
**Depends on.** Nothing in this list.
**Effort.** L — 4 adapters, real infra polling. Land SQS first as the proof.

### 2. WASM standalone runtime (`runtime = "wasm"`)

**Why.** The differentiator no Lambda emulator ships. Sub-ms cold start. Foundation for #3 + #4. WASI capability sandbox is the safe-execution moat.
**Acceptance.**
- `runtime = "wasm"` + `handler = "./path.wasm"` works in `riz.toml`
- Wasmtime host loads the module, configures WASI capabilities from `riz.toml` (`allowed_paths`, `allowed_hosts`, `clock_access`)
- Handler receives the same JSON envelope our other adapters use; returns the same response envelope
- No fs / net by default; opt-in only
- Cold start measured + asserted < 5ms in a bench
**Touches.** `src/process/wasm.rs` (new), `src/process/runtime.rs` (register), `Cargo.toml` (`wasmtime` dep), `assets/templates/wasm-http/` (new template), `tests/runtime_parity_*.rs` (extend), `benches/wasm_cold_start.rs`
**Depends on.** Nothing.
**Effort.** M — wasmtime is well-trodden; mostly wiring + protocol parity.

### 3. WASM pre-invoke guards (`guard_in`)

**Why.** Best demo riz can ship. Reuses #2's machinery. "Redact PII from any handler with a 4-line WASM module" is a viral one-liner.
**Acceptance.**
- `[function.X] guard_in = "./guards/validate.wasm"` works
- Guard runs against the incoming event envelope before the handler; can mutate or reject
- Rejection returns 400/403/etc. without invoking the handler
- One guard runs against handlers in Bun, Python, and Rust — proven by a shared-fixture test
- Guard timing surfaced in `/_riz/health` per guard
**Touches.** `src/server.rs` (guard step), `src/config.rs`, `src/state.rs` (guard timing field), `tests/wasm_guard_in_*.rs`
**Depends on.** #2.
**Effort.** S after #2.

### 4. WASM post-invoke guards (`guard_out`)

**Why.** The PII / secret-scrub story. Inverse of #3.
**Acceptance.**
- `[function.X] guard_out = "./guards/redact.wasm"` works
- Guard runs on the response envelope before bytes leave; can mutate or replace
- Same cross-runtime fixture test as #3
**Touches.** Same as #3 plus post-invoke hook in `src/server.rs`
**Depends on.** #2.
**Effort.** S after #2.

### 5. Node.js native runtime

**Why.** Broadens reach to shops that won't ship Bun in production. Same protocol as the Python adapter — line-delimited JSON over stdin/stdout.
**Acceptance.**
- `runtime = "node"` in `riz.toml` works
- New template `nodejs-http` scaffolds to `riz init`
- AWS-shape `handler = "index.handler"` works
- Reuses existing process-pool plumbing
- Parity test against the Bun runtime (same handler code, same response shape)
**Touches.** `src/process/node.rs` (new, modeled on `python.rs`), `src/process/runtime.rs`, `assets/templates/nodejs-http/`, `tests/node_runtime_*.rs`
**Depends on.** Nothing.
**Effort.** S — adapter pattern is well-established.

### 6. Go native runtime

**Why.** Same as #5 for Go shops. We already have the static-binary protocol (Rust uses it); Go is a thin wrapper.
**Acceptance.**
- `runtime = "go"` in `riz.toml` works
- New crate `crates/riz-go-runtime` (just a Go module, no Rust here) — minimal SDK that handles the JSON envelope
- New template `go-http` scaffolds
- Parity test against Rust runtime
**Touches.** `crates/riz-go-runtime/` (new), `src/process/rust.rs` extended OR `src/process/static_binary.rs` (generalize), `assets/templates/go-http/`
**Depends on.** Nothing — but consider refactoring Rust runtime to a generic "static binary" adapter first so Go reuses it cleanly.
**Effort.** S–M.

### 7. OpenTelemetry exporter

**Why.** Required for serious customer adoption. Traces span the full pipeline (route → guard → handler → response) and propagate W3C Trace Context. X-Ray headers come free.
**Acceptance.**
- `[otel]` config block with `endpoint`, `service_name`, `sampler`
- Spans for: request, guard.in, dispatch, handler.exec, guard.out, response
- W3C `traceparent` / `tracestate` headers honored on inbound + propagated to handler context
- Verified against a local OTLP collector in a test (or a mock collector)
**Touches.** `src/telemetry/otel.rs` (new), `src/server.rs` (span boundaries), `Cargo.toml` (`opentelemetry`, `opentelemetry-otlp`), `tests/otel_*.rs`
**Depends on.** Nothing in this list.
**Effort.** M.

### 8. LLM gateway — provider routing (Anthropic + OpenAI + Ollama)

**Why.** The single move that lands the "AI substrate" posture. Riz becomes the place every provider is one config block.
**Acceptance.**
- `[gateway]` config block: `default_provider`, `fallback_chain`, per-provider sub-blocks
- `ctx.invokeModel(name, prompt)` available in every runtime (Bun, Python, Rust, WASM)
- 3 providers: Anthropic, OpenAI, Ollama (Bedrock + Vertex follow as #8b)
- Fallback chain triggers on provider error
- One e2e test per provider hitting a mock endpoint
**Touches.** `src/gateway/{mod,anthropic,openai,ollama}.rs` (new), `src/process/*` (extend context API), per-runtime SDK additions (`crates/riz-rust-runtime`, `assets/templates/*/runtime`), `tests/gateway_*.rs`
**Depends on.** Nothing in this list — but #2/#5/#6 broaden the runtimes that benefit.
**Effort.** M.

### 9. LLM gateway — budget caps + cost telemetry

**Why.** Massive customer win — "show me a cost line per function in /_riz/health." Eliminates a chunk of what Langfuse / Helicone sell.
**Acceptance.**
- `budget_usd_24h` + `budget_usd_per_call` per function
- Per-call cost computed from provider pricing tables (kept as a Rust const map, refreshable)
- Cost surfaced in `/_riz/health` per-function: `cost_usd_24h`, `tokens_in`, `tokens_out`
- Budget exceeded → request rejected with structured error
**Touches.** `src/gateway/cost.rs` (new), `src/system/health.rs` (add fields), `src/config.rs`, `tests/gateway_budget_*.rs`
**Depends on.** #8.
**Effort.** S after #8.

### 10. LLM gateway — OpenAI-compatible endpoint

**Why.** Adoption multiplier. Any existing OpenAI client library "just works" against riz. Biggest non-MCP wedge into existing AI stacks.
**Acceptance.**
- `POST /_riz/v1/chat/completions` (and `models`, `embeddings`, `responses`) implemented
- Routes to whichever provider the gateway is configured for
- Streaming via SSE matches OpenAI's `data: ...\n\n` chunk format
- One e2e test using the official `openai` Python client against riz with `base_url=http://localhost:3000/_riz/v1`
**Touches.** `src/system/openai_compat.rs` (new), `src/gateway/mod.rs`, `tests/openai_compat_*.rs`
**Depends on.** #8.
**Effort.** S after #8.

### 11. MCP over WebSocket transport

**Why.** Streaming tool results need a real channel; HTTP POST doesn't fit. We already have WS infra. Wire `/_riz/mcp` over WS.
**Acceptance.**
- `WS /_riz/mcp` accepts the same JSON-RPC 2.0 envelopes
- Server-sent notifications work (tool progress, partial results)
- `riz mcp inspect` learns to use the WS transport
- Spec compliance verified by reading the 2025-11-25 Streamable HTTP section
**Touches.** `src/system/mcp/transport_ws.rs` (new), `src/ws/upgrade.rs` (extend), `src/main.rs` (mcp inspect), `tests/mcp_ws_*.rs`
**Depends on.** Nothing in this list.
**Effort.** S.

### 12. LLM gateway — streaming pass-through

**Why.** Long-running model calls need token-level streaming to feel right in MCP clients. Pair with #11.
**Acceptance.**
- `ctx.invokeModelStream(name, prompt)` yields tokens incrementally
- When called from an MCP tool over WS, tokens stream through to the client as partial tool results
- One e2e test: connect MCP over WS, call a tool that streams a model response, assert chunks arrive
**Touches.** `src/gateway/streaming.rs` (new), per-runtime SDKs, `src/system/mcp/dispatch.rs`
**Depends on.** #8, #11.
**Effort.** S after #11.

### 13. Record (capture)

**Why.** Foundational for #14 and for a v2 replay/eval. Cheap to ship, big future leverage.
**Acceptance.**
- `[record] enabled = true, sink = "sqlite:./riz.db"` works
- Every invocation persisted: function name, event envelope, response envelope, timings, downstream call list (start with HTTP calls only)
- `/_riz/recordings` returns paginated list + filter by function + time range
- Off by default; opt-in
**Touches.** `src/record/{mod,sqlite,sink}.rs` (new), `src/server.rs` (instrument), `Cargo.toml` (rusqlite), `tests/record_*.rs`
**Depends on.** Nothing.
**Effort.** M.

### 14. Dataset export

**Why.** Free fine-tune dataset once #13 is on. One command, ready-to-upload JSONL.
**Acceptance.**
- `riz export-dataset --function chat --since 30d --format openai > dataset.jsonl`
- Formats: `openai` (chat completion format), `anthropic` (messages format), `raw` (riz schema)
- One test per format
**Touches.** `src/main.rs` (new subcommand), `src/record/export.rs` (new), `tests/dataset_export_*.rs`
**Depends on.** #13.
**Effort.** S after #13.

### 15. Per-route MCP tool schemas

**Why.** Currently all routes for a function share one input schema. Path + query params are typed — tools should expose them.
**Acceptance.**
- A function with `routes = [GET /accounts/{id}]` generates an MCP tool whose `inputSchema` has `{ id: string }` typed from the path template
- Query params declared in `riz.toml` (or auto-inferred from a `[function.X.query]` block) become typed input fields
- Verified by `riz mcp inspect` output
**Touches.** `src/system/mcp/schema.rs` (extend), `tests/mcp_schema_*.rs`
**Depends on.** Nothing.
**Effort.** S.

### 16. Auto-derived MCP schemas from handler code

**Why.** Currently we infer from `riz.toml`. If we can read the TS types / Python annotations / Rust derives, the MCP tool descriptions stop being generic and become precise. LLMs tool-call dramatically better with precise schemas.
**Acceptance.**
- TS: parse handler signature via `swc` or `oxc`; emit JSON Schema for the inferred `event` / response types
- Python: walk the handler module via the `ast` module looking for type hints on the entry function
- Rust: read the crate's exported types via `cargo metadata` + a small attribute macro the riz-rust-runtime exposes
- Falls back to the generic schema when no types are detected
- Verified per runtime by `riz mcp inspect`
**Touches.** `src/system/mcp/schema_ts.rs`, `_py.rs`, `_rs.rs` (new), `crates/riz-rust-runtime/` (add derive macro)
**Depends on.** Nothing (but improvements across #5 / #6 broaden coverage).
**Effort.** L — three language paths. Land TS first.

---

## V1 shipping order — TL;DR

The order below treats Riz like a product release, not an engineering wish-list. Each phase produces something worth a blog post and a website update.

**Phase 1 — Breadth (lands "Riz is production-real," not a toy).**
Items: 1, 5, 6, 7 (Event sources, Node.js, Go, OpenTelemetry).
Outcome: covers the runtime stories shops actually ship with and the events Lambda is famous for. Demo: an SQS adapter feeding a Node.js handler with OTel traces.

**Phase 2 — WASM (lands "Riz is the safe runtime").**
Items: 2, 3, 4 (WASM runtime, then guards).
Outcome: ship one 4-line WASM that redacts SSNs from any handler. This is the viral demo.

**Phase 3 — LLM Gateway (lands "Riz is the AI control plane").**
Items: 8, 10, 9 (provider routing, OpenAI-compat, budget telemetry).
Outcome: point any OpenAI client at riz and it just works; cost surfaces in `/_riz/health`.

**Phase 4 — MCP polish + streaming (lands "Riz is the best agent surface").**
Items: 11, 12, 15, 16-TS (MCP over WS, streaming gateway, per-route schemas, TS auto-schemas).
Outcome: streaming tool results in Claude Code, precise tool input shapes from your TS types.

**Phase 5 — Recording (foundation for v2's eval / replay).**
Items: 13, 14 (capture + dataset export).
Outcome: every prod invocation captured; one command exports a fine-tune dataset.

After Phase 5, v1 ships. v2 begins with the replay/eval/cache stack.

---

## What I want from the next loop

When we resume after compact, I'll be looking for:

1. **Sign-off on the v1 cut** — is anything I marked v1 actually v2, or vice versa?
2. **Sign-off on the phase order** — does shipping breadth-first (#1) before WASM feel right, or should WASM lead?
3. **One pick** to start. Default would be #1 (Event sources, SQS adapter first) since it has the highest customer pull and zero dependencies on the rest of v1.

Once a phase is picked, I'll write the implementation plan for the lead item and start iterating in the loop pattern we've been using.
