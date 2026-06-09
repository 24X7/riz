# Riz v1 Roadmap — Ranked, Shovel-Ready (Revised)

**Status:** plan of record · written 2026-06-08 · supersedes the AWS-event-sources draft
**Goal:** Take v0.1 (Lambda runtime + MCP server) and ship v1 as the self-hosted, agent-native Lambda substrate. 13 items, each mapped to a real industry pattern or formal spec. Compound subsystems (embeddings, eval scoring, OAuth, federation, stateful memory) push to v2.

---

## Direction

Riz v0.1 is the Lambda runtime your agent can call. Riz v1 is the runtime your agent can **build against** — same HTTP/WS wire contract, plus WASM-sandboxed code, an LLM gateway every existing client speaks to, audit-grade event reporting, two more native runtimes, and the MCP polish that makes LLM tool-calling actually accurate.

**Two rules.**
1. **APIs only.** HTTP API v2 + WebSocket are the only ingress shapes riz speaks. SQS / SNS / S3 / EventBridge adapters are explicitly OUT of scope — this is a durable user directive in CLAUDE memory.
2. **Atomic shipments.** Each item is one config block, one subsystem, one test file. Anything that needs two new subsystems to land usefully is v2.

---

## In v1 (13 items — #14 deferred to v2; #5 shipped)

| # | Item | Category | Effort |
|---|---|---|---|
| 1 | Event reporting / observability emission | Observability — business events | M |
| 2 | WASM standalone runtime (`runtime = "wasm"`) | Sandboxing | M |
| 3 | WASM pre-invoke guards (`guard_in`) | Sandboxing — middleware | S after #2 |
| 4 | WASM post-invoke guards (`guard_out`) | Sandboxing — middleware | S after #2 |
| 5 | Node.js native runtime ✅ **SHIPPED** | Runtime breadth | S |
| 6 | Go native runtime | Runtime breadth | S–M |
| 7 | OpenTelemetry exporter (infra spans) | Observability — distributed tracing | M |
| 8 | LLM gateway — provider routing ✅ **SHIPPED** | AI Gateway | M |
| 9 | LLM gateway — budget caps + cost telemetry ✅ **SHIPPED** | AI FinOps | S after #8 |
| 10 | LLM gateway — OpenAI-compatible endpoint ✅ **SHIPPED** | Industry standard interop | S after #8 |
| 11 | MCP Streamable HTTP — SSE streaming | MCP spec compliance | S |
| 12 | MCP progress notifications during tool call | MCP spec compliance | S after #11 |
| 13 | Per-route MCP tool schemas | MCP polish | S |

## Out of v1 (deferred or out of scope)

| Item | Why deferred / dropped |
|---|---|
| #14 Auto-derived MCP schemas from handler code | Violates the atomic-shipment rule — needs 3 independent language parsers (TS oxc/swc, Python `ast`, Rust proc-macro). #13's per-route schemas capture ~80% of the tool-calling-accuracy win at S effort. v2. |
| AWS event sources (SQS / SNS / S3 / EventBridge) | OUT OF SCOPE — durable directive: "AWS HTTP/WS Lambdas only." |
| Record (raw capture, dataset export) | Pure recording without replay/eval is just a log file. Captured-envelope persistence folds into #1; replay + dataset export wait for v2 when scoring + dispatcher reuse ship. |
| Replay CLI (`riz replay`) | Needs capture + a dispatcher entry point for synthetic invocations + a diff renderer. v2. |
| Agentic test loop | Needs Replay + a regression-rank algorithm. v2. |
| Semantic-similarity cache | Needs an embeddings provider + a similarity index. Whole new subsystem. v2. |
| Prompt versioning | Needs a config schema + filesystem layout for prompt branches. v2. |
| Eval harness (`riz eval`) | Needs Prompt versioning + Gateway + per-domain scoring rubric. The heaviest. v2. |
| A/B win rate in `/_riz/health` | Needs Eval. v2. |
| OAuth 2.1 + RFC 8707 Resource Indicators | New auth subsystem; bearer-token covers the v1 buyer. v2. |
| MCP federation | Needs discovery protocol + multi-instance lifecycle. v2. |
| WebSocket as a second MCP transport | Solves no problem today, unlocks zero capability, YAGNI. ~1-day add when a concrete client need appears. See item #11 scope decision. |
| Stateful agent memory (`/_riz/memory/{agent_id}`) | Out of scope per user — separate "agent state layer" project. |
| Java / JVM runtime | Out of scope per user. |

---

## Competitor / industry map (single reference)

| Pattern | OSS leader | Cloud / commercial | Spec | Riz position |
|---|---|---|---|---|
| Lambda emulators | LocalStack, SAM local, serverless-offline | n/a | AWS Lambda + API Gateway v2 | The only one with MCP-native + on-box safety + WASM |
| WASM runtime | wasmtime (Bytecode Alliance) | Fastly Compute, Cloudflare Workers, Wasmer Edge, Fermyon Spin, wasmCloud | WASI Preview 2 (stable late 2024) | Lambda-shape WASM with WASI capabilities — none of the Lambda emulators ship this |
| Cross-runtime guards / middleware | Envoy WASM filters, Istio WASM extensions, OPA | Lambda Layers (no sandbox), AWS WAF | n/a | Pre/post WASM guards as Lambda middleware — first of its kind |
| AI Gateway | LiteLLM (BerriAI, ~20k★) | Cloudflare AI Gateway, Vercel AI Gateway, Portkey, Helicone, OpenRouter | n/a — convergent pattern | The OSS, self-host, single-binary, Rust slot. LiteLLM is python-proxy. Portkey is Node. Nothing fast + integrated with execution. |
| AI FinOps (budget + cost) | Langfuse | Helicone, Portkey, Datadog LLM Observability | n/a | Cost surfaces in `/_riz/health` next to latency — same engineer surface |
| OpenAI API compat | Ollama, vLLM, llama.cpp server, LM Studio | Anthropic OpenAI-compat endpoint (2024), Together AI, Anyscale, Fireworks | De-facto since 2023 | Make every existing OpenAI client work against riz |
| Distributed tracing | OpenTelemetry (CNCF graduated 2024) | Datadog APM, Honeycomb, New Relic, AWS X-Ray | OTLP, W3C Trace Context | Standard OTLP exporter — table stakes |
| Structured event emission | Vector (Datadog, Rust), Fluentd/Fluent Bit | Segment, Honeycomb, Sentry, AWS EventBridge | OTel Logs signal (stable 2024) | Per-invocation business events to configurable sinks |
| MCP transport | MCP Inspector, Anthropic reference servers | Cline, Cursor, Claude Code | MCP 2025-11-25: Streamable HTTP (POST + GET-SSE) | Spec-compliant transport including server-initiated SSE |
| MCP schema quality | tRPC, Zod, Pydantic (adjacent patterns) | n/a | Anthropic + OpenAI tool-calling guidance | Auto-derive tool schemas from handler types — ~30% accuracy lift per published research |

---

## Per-item shovel-ready specs

Each entry: **Industry context** · **Why we care** · **Why you care** · **Acceptance** · **Touches** · **Depends on** · **Effort**.

---

### 1. Event reporting / observability emission

**Industry context.** Distinct from infra tracing (#7) and from log aggregation. The pattern is **structured business events** — one record per logical operation, ready for audit, analytics, FinOps, or replay.
- OSS routers: Vector (Datadog, written in Rust), Fluentd, Fluent Bit. These move events; they don't define what an event is.
- Hosted: Segment (product events), Honeycomb (high-cardinality events), Sentry (error events), AWS EventBridge (system events).
- Emerging spec: **OpenTelemetry Logs signal** (stable 2024) defines a structured log record shape with severity, attributes, and trace_id correlation. Our events fit this.

**Why we care.** This is the surface every audit-grade Lambda customer builds themselves on top of CloudWatch. If riz emits the event for them — with cost, latency, trace_id, redacted I/O, correlation id — we replace a quarter of an internal "observability platform" team's roadmap. Also lays the groundwork for v2 replay (#13/#14-style capture lives here).

**Why you care.** Every `riz run` produces a structured event per invocation, shippable to whatever sink you already have. Audit log, compliance log, business analytics, and FinOps feed off one stream. You don't write a custom logger again.

**Acceptance.**
- `[events]` block in `riz.toml` with `enabled = true`, `sinks = ["stdout", "webhook", "syslog", "datadog", "s3", "otlp"]`
- One event per invocation: `{ts, function, route, method, status, latency_ms, cost_usd, trace_id, correlation_id, runtime, input_redacted, output_redacted, guard_verdicts}`
- Redaction rules in `[events.redact]` — `field_paths`, `regex_patterns`
- Sink failure isolated (one broken webhook doesn't block the runtime)
- Off by default; opt-in
- Tests: one per sink against a fixture endpoint

**Touches.** `src/events/{mod,sink_stdout,sink_webhook,sink_syslog,sink_datadog,sink_s3,sink_otlp}.rs` (new), `src/server.rs` (emit hook in request/response path), `src/config.rs` (new types), `src/system/health.rs` (event counters), `tests/events_*.rs`

**Depends on.** Nothing.
**Effort.** M.

---

### 2. WASM standalone runtime (`runtime = "wasm"`)

**Industry context.**
- OSS runtime: **wasmtime** (Bytecode Alliance reference impl, what we'd use)
- WASM platforms / hosts: Fastly Compute@Edge (commercial, GA 2021), Cloudflare Workers (supports WASM), Wasmer Edge (commercial hosting), Fermyon Spin (OSS app framework), wasmCloud (OSS, CNCF), WasmEdge (CNCF)
- Lambda emulators (LocalStack, SAM local, serverless-offline): **none support WASM** — the gap riz fills
- Formal spec: **WASI Preview 2 (component model)**, stable late 2024. Capability-based: filesystem, network, clock, env granted explicitly.

**Why we care.** "Lambda emulator with WASM" is a category of one. Sub-ms cold start means we can talk about agents running code without paying cloud-cold-start tax. The capability sandbox is the safety story that #3 + #4 build on.

**Why you care.** Drop a `.wasm` in, point `riz.toml` at it, get a handler that boots in sub-ms with zero fs/net unless you say so. Ship the same `.wasm` to Linux / macOS / edge. Safe execution of untrusted code (LLM-generated, third-party) without spinning up containers.

**Acceptance.**
- `runtime = "wasm"` + `handler = "./path.wasm"` works
- wasmtime host loads the module, applies WASI capabilities from `riz.toml` (`allowed_paths`, `allowed_hosts`, `clock_access`, `env_vars`)
- Handler receives the same JSON event envelope as Bun/Python/Rust; returns same response envelope
- No fs / no net / no clock by default
- Cold start measured + asserted < 5ms in a bench
- New template `wasm-http` for `riz init`

**Touches.** `src/process/wasm.rs` (new), `src/process/runtime.rs` (register), `Cargo.toml` (`wasmtime`, `wasmtime-wasi`), `assets/templates/wasm-http/`, `tests/runtime_parity_wasm.rs`, `benches/wasm_cold_start.rs`
**Depends on.** Nothing.
**Effort.** M.

---

### 3. WASM pre-invoke guards (`guard_in`)

**Industry context.** Cross-runtime middleware / policy as code.
- Envoy WASM filters, Istio WASM extensions — sidecar-side WASM, HTTP-only, service-mesh framing
- Open Policy Agent (OPA) — JSON-policy engine, not WASM, used for authz
- AWS Lambda Layers — middleware without sandboxing; you trust the layer
- AWS WAF — managed rules, not user-extensible at runtime
- E2B, Modal — sandboxed exec for agent code; closest peer for "untrusted code" framing but at a different layer

**Why we care.** Riz is the first Lambda-shape runtime where a WASM module sits between the wire and the handler. One guard works across Bun, Python, Rust, WASM — that cross-runtime property is a feature nobody else can match because nobody else has the polyglot pool. Demo line: "Redact a SSN from any handler with a 4-line WASM."

**Why you care.** Write the validation / injection-detection / rate-limit logic once, in WASM, deploy in front of every handler regardless of language. Same guard protects Bun and Python and Rust. No SDK in three languages.

**Acceptance.**
- `[function.X] guard_in = "./guards/validate.wasm"` works
- Guard runs against the incoming event before the handler
- Guard can mutate the event (e.g. scrubbed payload) or reject (status + reason)
- Rejection returns the chosen status without invoking the handler
- Cross-runtime fixture test: same guard wraps a Bun, Python, Rust, and WASM handler
- Guard timing surfaces per guard in `/_riz/health`

**Touches.** `src/server.rs` (guard step in request path), `src/config.rs`, `src/system/health.rs`, `tests/wasm_guard_in_*.rs`
**Depends on.** #2.
**Effort.** S after #2.

---

### 4. WASM post-invoke guards (`guard_out`)

**Industry context.** Same as #3, post-response. PII-redaction / secret-scrub / response-shape enforcement. AWS PII detection is a separate Comprehend service call; this is in-process and free.

**Why we care.** Pairs with #3 to complete the safety story. The PII redaction demo lands directly on the security-conscious buyer.

**Why you care.** Final response sweep — redact emails, scrub tokens, enforce that your handler can't accidentally leak an internal field. One `.wasm`, works for every handler.

**Acceptance.**
- `[function.X] guard_out = "./guards/redact.wasm"` works
- Guard runs on the response envelope before bytes leave
- Can mutate (replace fields) or replace (full envelope swap)
- Cross-runtime fixture test as in #3
- Guard timing in `/_riz/health`

**Touches.** Same as #3 plus post-invoke hook
**Depends on.** #2.
**Effort.** S after #2.

---

### 5. Node.js native runtime — ✅ SHIPPED (2026-06-08)

**Shipped.** `runtime = "node"` works via `src/process/node.rs` +
`assets/node-adapter.mjs` (ESM, `pathToFileURL` dynamic import). `nodejs-http`
template + `riz init` support, `doctor` check for `node`, `echo-node` example
(+ README), wired into `riz.all.toml` / `smoke-all.sh`. Full cross-runtime
parity matrix vs Bun: echo, errors, response, verbs, context, request_shape,
binary (`tests/runtime_parity_*.rs`, all green).

**Industry context.** Node is the #1 production Lambda runtime by share (AWS public stats: ~50%+ of Lambdas). Bun is fast but enterprise won't ship it for compliance (no LTS, no FedRAMP, single-vendor). Riz today supports Bun for TS only — we're missing the actual production runtime.

The adapter pattern is the same as our Python adapter — line-delimited JSON over stdin/stdout, no new design.

**Why we care.** Without Node, riz is "interesting but my employer won't let me ship it." With Node, we land in the production Lambda buyer's funnel.

**Why you care.** Existing Node.js Lambda code drops in unmodified. Same `index.handler` AWS shape. No rewrite, no Bun mandate.

**Acceptance.**
- `runtime = "node"` in `riz.toml` works
- New template `nodejs-http` via `riz init`
- AWS `handler = "index.handler"` works
- Parity test against the Bun runtime: same handler code, identical response

**Touches.** `src/process/node.rs` (new, model on `python.rs`), `src/process/runtime.rs`, `assets/templates/nodejs-http/`, `tests/node_runtime_*.rs`
**Depends on.** Nothing.
**Effort.** S.

---

### 6. Go native runtime

**Industry context.** Go is the #2–#3 Lambda runtime by share. Already a static binary — riz's Rust adapter pattern reuses ~90% of the path. Need a thin `riz-go-runtime` Go module (SDK) + `runtime = "go"` registration.

**Why we care.** Completes the realistic-production-runtime trio (Node + Python + Go + Rust). Wins the "ops cares about Go, dev cares about TS" account.

**Why you care.** Existing Go Lambda code drops in. `lambda.Start(handler)` semantics preserved through riz's Go SDK.

**Acceptance.**
- `runtime = "go"` works
- New `crates/riz-go-runtime` directory (a Go module, not a Rust crate — the name's a holdover from the layout) shipping a minimal SDK
- New template `go-http` via `riz init`
- Parity test against the Rust runtime

**Touches.** `crates/riz-go-runtime/` (new Go module), generalize `src/process/rust.rs` → `src/process/static_binary.rs` so Go reuses it cleanly, `assets/templates/go-http/`
**Depends on.** Nothing — but consider the static-binary refactor first.
**Effort.** S–M.

---

### 7. OpenTelemetry exporter (infra spans)

**Industry context.**
- **OpenTelemetry** — CNCF graduated 2024, multi-signal (traces, metrics, logs). De-facto industry standard.
- Backends consuming OTLP: Datadog APM, Honeycomb, New Relic, Tempo, Jaeger, Lightstep
- AWS X-Ray — proprietary, but riz emits W3C `traceparent` so X-Ray ingests for free
- Distinction from #1: OTel here = infra spans (request → guard → dispatch → handler → response); #1 = business events (one event per invocation with cost / correlation / audit)

**Why we care.** Every serious customer asks "does it speak OTel?" before "does it scale?". This is table-stakes observability. Pairs with #1 to give the runtime both layers — distributed traces AND business events.

**Why you care.** Point your existing OTLP collector at riz, get spans for every step of every invocation in your existing Datadog/Honeycomb dashboard. W3C trace context propagated to your handler so downstream calls correlate.

**Acceptance.**
- `[otel]` config block: `endpoint`, `service_name`, `sampler`
- Spans for: request, guard_in, dispatch, handler.exec, guard_out, response
- W3C `traceparent` + `tracestate` honored on inbound + propagated into handler context
- Verified against a mock OTLP collector in tests

**Touches.** `src/telemetry/otel.rs` (new), `src/server.rs` (span boundaries), `Cargo.toml` (`opentelemetry`, `opentelemetry-otlp`, `opentelemetry-sdk`), `tests/otel_*.rs`
**Depends on.** Nothing in this list.
**Effort.** M.

---

### 8. LLM gateway — provider routing

**Industry context.** The "AI Gateway" category. No formal spec yet — convergent pattern across:

| Player | Type | Lang | Notable |
|---|---|---|---|
| **LiteLLM (BerriAI)** | OSS, ~20k★ | Python proxy | De-facto OSS standard; SDK + standalone proxy |
| **Cloudflare AI Gateway** | Managed cloud | Edge JS | GA 2024, free tier, ties into Workers |
| **Vercel AI Gateway** | Managed cloud | Edge JS | Launched 2024, ties into AI SDK |
| **Portkey** | Commercial SaaS + OSS gateway | Node.js | Routing + retries + observability |
| **Helicone** | Commercial SaaS | TS | Observability-first; gateway second |
| **OpenRouter** | Hosted API aggregator | n/a | One key, every model — hosted only |
| Anthropic MCP | Spec | n/a | Different layer — tool protocol, not gateway |

**The OSS, self-host, fast, single-binary, Rust slot is empty.** LiteLLM is python-proxy-as-service (often a bottleneck). Portkey OSS is Node.js. Cloudflare + Vercel are cloud. Riz can be the one OSS gateway that's also a Lambda runtime — co-located with handler execution, no extra hop.

**Why we care.** This is the single move that lands the "AI substrate" posture. We stop being a Lambda emulator with MCP; we become the place every LLM call goes through.

**Why you care.** One config block: providers, fallback chain, defaults. Your handler calls `ctx.invokeModel("claude-sonnet-4-6", prompt)`. Switching to GPT or Ollama is a config edit. Same surface for every runtime — Bun, Python, Rust, WASM.

**Acceptance.**
- `[gateway]` config: `default_provider`, `fallback_chain`, per-provider sub-blocks
- `ctx.invokeModel(name, prompt)` in every runtime
- 3 providers in v1: Anthropic, OpenAI, Ollama (Bedrock + Vertex follow)
- Fallback chain on provider error
- One e2e test per provider against a mock

**Touches.** `src/gateway/{mod,anthropic,openai,ollama}.rs` (new), per-runtime SDK additions (`crates/riz-rust-runtime`, Python adapter, Bun adapter, Node adapter), `tests/gateway_*.rs`
**Depends on.** Nothing in this list — but #6/#2 broaden the runtimes that benefit (#5 Node shipped).
**Effort.** M.

---

### 9. LLM gateway — budget caps + cost telemetry

**Industry context.** "AI FinOps." Helicone leads commercially, Langfuse is the OSS contender, Portkey is the routing-plus-cost player. No formal spec — pattern is per-provider pricing tables → per-call cost → surface in dashboards. Datadog is bolting LLM Observability onto APM (commercial only).

**Why we care.** Eliminates a chunk of what Helicone / Langfuse / Portkey charge for. Cost in `/_riz/health` next to invocations means the engineer who already checks latency sees cost on the same page — no separate dashboard, no separate vendor.

**Why you care.** "Budget exceeded → request rejected" guardrail. Per-function cost line in the same surface you check for latency. Token in/out per function. No need to instrument your handler.

**Acceptance.**
- `budget_usd_24h` + `budget_usd_per_call` per function
- Per-call cost computed from provider pricing tables (Rust const map, refreshable)
- `/_riz/health` adds per-function `{cost_usd_24h, tokens_in, tokens_out, budget_remaining}`
- Budget exceeded → structured rejection (412 Precondition Failed + reason)

**Touches.** `src/gateway/cost.rs` (new with pricing tables), `src/system/health.rs` (new fields), `src/config.rs`, `tests/gateway_budget_*.rs`
**Depends on.** #8.
**Effort.** S after #8.

---

### 10. LLM gateway — OpenAI-compatible endpoint

**Industry context.** OpenAI's `/v1/chat/completions`, `/v1/embeddings`, `/v1/models`, `/v1/responses` shapes are the de-facto industry standard since 2023. Every inference engine ships it:

| Engine | OpenAI endpoint? |
|---|---|
| Ollama | Yes, built-in |
| vLLM | Yes (`--api-server`) |
| llama.cpp server | Yes |
| LM Studio | Yes |
| Together AI, Anyscale, Fireworks, Groq | Yes |
| **Anthropic** | Yes — launched 2024 (`api.anthropic.com/v1/openai/`) |
| Cloudflare Workers AI | Yes |
| LiteLLM's primary feature | Translate any provider → OpenAI shape |

By shipping `/_riz/v1/chat/completions`, riz inherits **every existing OpenAI client** — Python `openai`, JS `openai`, LangChain, LlamaIndex, AutoGen, CrewAI, every agent framework, every notebook in Kaggle. Just set `base_url=http://localhost:3000/_riz/v1`. No code changes.

**Why we care.** Single biggest adoption multiplier on the v1 list. Without this, riz is a runtime you build against. With this, riz is a runtime every existing AI stack already builds against.

**Why you care.** Point your existing OpenAI client at riz, every routing + budget + cache feature in the gateway just works. No SDK migration. No code change. Drop-in.

**Acceptance.**
- `POST /_riz/v1/chat/completions` — supports streaming via SSE matching OpenAI's `data: ...\n\n` chunks
- `POST /_riz/v1/embeddings` — routes to the configured embeddings provider
- `GET /_riz/v1/models` — lists configured models
- One e2e test using the official `openai` Python client against riz with `base_url`

**Touches.** `src/system/openai_compat.rs` (new), `src/gateway/mod.rs`, `tests/openai_compat_*.rs`
**Depends on.** #8.
**Effort.** S after #8.

---

### 11. MCP Streamable HTTP — SSE streaming (HTTP transport only)

**Industry context.** Formal MCP spec 2025-11-25 mandates **Streamable HTTP** as the standard transport:
- POST for client → server (current riz behavior)
- **GET → SSE for server → client** (currently missing in riz)
- WebSocket was considered and not adopted in the spec

Real implementations using this transport:
- Anthropic's reference servers
- MCP Inspector (the official testing client)
- Cline, Cursor, Claude Code

Server can multiplex notifications + responses on a single SSE channel, keyed by JSON-RPC id.

**Scope decision: HTTP only in v1.** WebSocket-as-MCP-transport is technically possible (riz already has WS infrastructure for user chat handlers) but explicitly out of scope here for three reasons:

1. **Solves no problem today.** Every real MCP client speaks Streamable HTTP. Shipping WS now is building for hypothetical demand.
2. **Unlocks zero capability.** Anything WS could carry, HTTP+SSE already carries. We'd pay maintenance + auth + docs cost forever in exchange for nothing.
3. **YAGNI applies cleanly.** Riz is greenfield. When someone shows up with a real WS use case (probably browser-based agent doing rapid multi-tool dispatch), it's a ~1-day add. Pay that cost then, not now.

This is a deliberate scope choice, not an oversight. Re-evaluate when a real client request arrives.

**Why we care.** Riz today does POST request/response. Without the GET-SSE upgrade we can't push tool progress, partial results, or server-initiated capability changes — basic spec compliance. This is the smallest path to "fully conformant 2025-11-25 server."

**Why you care.** Long-running tools (codegen, multi-step searches, model streaming) feel right in your client. Progress bars work. Partial results render. No timeouts on 30-second tool calls.

**Acceptance.**
- `GET /_riz/mcp` returns an SSE stream (currently returns 405 — change to 200 with `Content-Type: text/event-stream`)
- Notifications multiplexed by JSON-RPC id
- `Mcp-Session-Id` header honored for client session correlation
- `riz mcp inspect` learns to use the SSE channel
- Verified against MCP Inspector

**Touches.** `src/system/mcp/transport.rs` (new), `src/system/mcp/mod.rs` (GET handler), `src/main.rs` (mcp inspect), `tests/mcp_sse_*.rs`
**Depends on.** Nothing.
**Effort.** S.

---

### 12. MCP progress notifications during tool call

**Industry context.** Formal MCP spec section: `notifications/progress`. Used for incremental tool results during long-running operations. Pairs with #11 — without SSE, no way to deliver them.

When a riz function calls the gateway with `invokeModelStream`, each token turns into a `notifications/progress` message; Claude Code shows it incrementally.

**Why we care.** Closes the loop from #8/#11/#12: gateway streams tokens, MCP transport pushes them as progress notifications, the agent's UI renders them live. Without this, "streaming" is a marketing claim, not a wire reality.

**Why you care.** A handler that calls Claude through the gateway and emits a long response renders one character at a time in Claude Code, not as a 10-second-then-blob.

**Acceptance.**
- `notifications/progress` emitted from a tool call when the gateway is in streaming mode
- Progress token (per spec) carried correctly so client can correlate
- e2e test: connect over SSE, call a tool that calls `invokeModelStream`, assert chunks arrive in order with the right correlation token

**Touches.** `src/gateway/streaming.rs`, `src/system/mcp/dispatch.rs`, `tests/mcp_progress_*.rs`
**Depends on.** #8, #11.
**Effort.** S after #11.

---

### 13. Per-route MCP tool schemas

**Industry context.** No specific competitor — this is about MCP tool quality. Today riz tools have a single generic input schema (`{body, headers, queryParams, pathParams, route, isBase64Encoded}`). Better tool descriptions = LLMs tool-call them better. Anthropic + OpenAI published research showing **~30% accuracy improvement** with precise input schemas.

**Why we care.** Without this, LLMs over-rely on description text and miss obvious typed inputs. Precise per-route schemas are the difference between "tool works sometimes" and "tool is reliable."

**Why you care.** Your `GET /accounts/{id}` tool exposes `{ id: string }` as a typed input — not buried inside a generic `pathParams` map. Claude calls it correctly first time, every time.

**Acceptance.**
- A function with `routes = [GET /accounts/{id}]` generates a tool whose `inputSchema` includes `{ id: string }` typed from the path template
- Query params declared in `riz.toml` (or auto-inferred from a `[function.X.query]` block) become typed input fields
- Verified by `riz mcp inspect` output + an explicit schema-shape test

**Touches.** `src/system/mcp/schema.rs` (extend), `src/config.rs` (optional query block), `tests/mcp_schema_per_route_*.rs`
**Depends on.** Nothing.
**Effort.** S.

---

### 14. Auto-derived MCP schemas from handler code — ⏬ DEFERRED TO v2

**Why deferred (2026-06-08):** the only v1 item that breaks the "atomic shipment"
rule — it needs three independent language parsers, and even the TS-first slice
is L effort. #13 (per-route schemas, S) delivers most of the tool-calling-accuracy
win first. Build #13 in v1; revisit auto-derivation in v2.

**Industry context.** "Schema-as-types" pattern. tRPC, Zod, Effect-TS, Pydantic, attrs — all extract type info to produce runtime schemas. Anthropic + OpenAI both publish guidance: precise tool input schemas materially improve LLM tool-calling accuracy.

Three language paths:
- TS via `oxc` or `swc` parsing (Rust-native, fast)
- Python via `ast` module + type hints
- Rust via attribute macro on the handler signature (riz-rust-runtime crate adds `#[riz::handler]`)

**Why we care.** This is what separates riz tools from generic "call this Lambda" tools. When the schema reflects the actual TS / Python / Rust types you wrote, the LLM stops guessing. Combined with #13 this is the MCP-quality story.

**Why you care.** Write your handler in TypeScript with proper types — riz picks them up automatically and presents them to Claude. No JSON-schema babysitting, no `riz.toml` schema blocks if you don't want them.

**Acceptance.**
- TS path: parse handler signature via `oxc` or `swc`, emit JSON Schema for the inferred `event` parameter type + response type
- Python path: walk the handler module via `ast`, read type hints
- Rust path: `#[riz::handler]` attribute macro on the entry function emits a `riz_schema()` const the runtime reads at registration
- Falls back to #13's generic schema when no types are detected
- Verified per runtime by `riz mcp inspect`

**Touches.** `src/system/mcp/schema_ts.rs`, `_py.rs`, `_rs.rs` (new), `crates/riz-rust-runtime/` (add derive macro), `tests/mcp_schema_auto_*.rs`
**Depends on.** Nothing (but coverage broadens with #5 / #6).
**Effort.** L — three language paths. Ship TS first (largest user base), Python second, Rust last.

---

## V1 shipping order — TL;DR

The phases group items that share machinery and produce a coherent narrative each release. Each phase is worth a blog post and a website update.

**Phase 1 — Observability + breadth (lands "Riz is production-real, not a toy").**
Items: 1, 5, 6, 7. (Event reporting, Node.js, Go, OpenTelemetry.)
Outcome: realistic runtime support + audit-grade events + standard OTLP. Removes the "but does it speak our stack?" objection.

**Phase 2 — WASM (lands "Riz is the safe runtime").**
Items: 2, 3, 4. (WASM runtime → guards.)
Outcome: ship one 4-line WASM that redacts a SSN from any handler regardless of runtime. This is the viral demo.

**Phase 3 — LLM Gateway (lands "Riz is the AI control plane").**
Items: 8, 10, 9. (Provider routing → OpenAI-compat → budget telemetry.)
Outcome: existing OpenAI clients just work; cost surfaces in `/_riz/health`. The single biggest adoption multiplier on the list (#10) ships in this phase.

**Phase 4 — MCP spec compliance + polish (lands "Riz is the best agent surface").**
Items: 11, 12, 13, 14-TS. (SSE transport, progress notifications, per-route schemas, TS auto-schemas.)
Outcome: streaming tool results, spec-compliant 2025-11-25 server, precise tool input shapes from your code.

v1 ships here. v2 begins with replay / eval / semantic cache / OAuth / federation / Python+Rust auto-schemas.

---

## Status & build order (live)

**Shipped (2026-06-08/09):**
- #5 Node.js runtime — full cross-runtime parity matrix vs Bun.
- **WS process-pool hardening** — `invoke_generic` liveness/respawn parity,
  `$connect` query-param parsing, `concurrency = 0` rejection (all TDD).
- **#8 + #9 + #10 — the LLM gateway** (the whole AI-gateway slice): provider
  routing + fallback, OpenAI-compatible `/_riz/v1/{chat/completions,embeddings,
  models,usage}`, SSE streaming, mock + OpenAI + Ollama + Anthropic providers,
  budget caps + cost telemetry. All TDD; demonstrated live in demo.sh.

**Cut refined:** #14 (auto-derived MCP schemas, L) deferred to v2 — the only item
that breaks the atomic-shipment rule; #13 captures most of the value at S. v1 = 13.

**Remaining v1 build order** (re-ranked for production-readiness + mass appeal +
single-binary):

1. **#2 → #3 → #4** WASM runtime → guards — the category-defining differentiator
   ("Lambda emulator with WASM" = category of one); self-contained (wasmtime in-binary).
2. **#1** Event reporting (zero deps, production table-stakes; foundation for #7 + v2 replay).
3. **#11 → #12** MCP Streamable HTTP / SSE → progress notifications (cheap spec wins).
4. **#13** Per-route MCP tool schemas (~30% tool-calling accuracy lift, S effort).
5. **#7** OpenTelemetry exporter (infra spans; table-stakes observability).
6. **#6** Go native runtime (after the static-binary refactor).

Each loop iteration: show the plan + what's next, build the lead item end-to-end
with tests, keep slop out, and re-rank as reality changes.
