# riz · Capability Card

> **Lambda for the agent era. Your APIs become MCP tools.**
> Self-hosted AWS Lambda runtime in one ~10 MB Rust binary.

---

## Identity

| | |
|---|---|
| **What it is** | Self-hosted AWS HTTP API v2 + WebSocket Lambda runtime |
| **The hook** | The moment `riz run` boots, every function is a typed MCP tool an agent can call |
| **Binary size** | ~10 MB · single static Rust binary · no Docker · no GC pauses |
| **License** | Apache-2.0 |
| **Links** | [github.com/24X7/riz](https://github.com/24X7/riz) · [riz.dev](https://riz.dev) |

---

## Runtimes

| Runtime | Notes |
|---|---|
| **Bun** | TypeScript / JavaScript handlers |
| **Node.js** | JS handlers via `node` |
| **Python** | `python3` — same `index.handler` resolution |
| **Rust** | Pre-compiled native binary |
| **WASM** | `wasm32-wasip1` under wasmtime · WASI deny-by-default fs/net · capabilities granted explicitly in `riz.toml` |

All five are **cross-runtime parity-tested** — every HTTP capability (verbs, path params, headers, cookies, binary bodies, stage variables) gets an identical response across all runtimes.

---

## Protocol Surface

| Protocol | Detail |
|---|---|
| **HTTP API Gateway v2** | Exact `aws_lambda_events` types · all 7 verbs · `{id}` / `{proxy+}` paths · `$default` catch-all · stage variables · real Lambda context |
| **WebSocket APIs** | `$connect` / `$disconnect` / `$default` lifecycle · `@connections` management API (GET / POST / DELETE / LIST) for server→client push |
| **Wire compat** | Handlers move between AWS and riz unchanged — same `index.handler` resolution, same request IDs, same `getRemainingTimeInMillis` |

---

## Agent-Native / MCP

| Item | Detail |
|---|---|
| **Endpoint** | `/_riz/mcp` — always on, no extra config |
| **Spec** | MCP 2025-11-25 (also negotiates 2024-11-05 / 2025-03-26 / 2025-06-18) |
| **Transport** | JSON-RPC 2.0 over Streamable HTTP |
| **Tool registration** | Every function in `riz.toml` auto-registers with typed input + output schemas |
| **SDK lines required** | Zero |
| **Inspect** | `riz mcp inspect` — initialize + tools/list one-screen report |

```bash
claude mcp add riz-local --transport http http://localhost:3000/_riz/mcp
```

---

## LLM Gateway

| Item | Detail |
|---|---|
| **Endpoint** | `/_riz/v1/*` — OpenAI-compatible |
| **Providers (shipped)** | OpenAI · Anthropic · Ollama (local) · mock (deterministic, no network) |
| **Routing** | Model-prefix (`anthropic/claude-sonnet-4-6`) or `default_provider` with de-duplicated fallback chain |
| **Streaming** | SSE chunks (`stream: true`) |
| **Embeddings** | `POST /_riz/v1/embeddings` |
| **FinOps** | `budget_usd` cap → HTTP 412 on breach · `GET /_riz/v1/usage` — cumulative cost + tokens per provider |

---

## Auth & Security

| Layer | Detail |
|---|---|
| **Lambda authorizers** | REQUEST (calls a user function) + JWT (JWKS URL, TTL cache) |
| **CORS** | Auto-preflight · OPTIONS → 204 · origin echo · attacker-origin rejection · global `[cors]` + per-function override |
| **Admin gating** | Bearer-token (`RIZ_AUTH_BEARER_TOKEN`) on `/_riz/*`; `/_riz/health` stays open for probes |
| **Always-on child safety** | `RLIMIT_CORE=0` · FD caps · fork-bomb caps · no privilege escalation · `PR_SET_NO_NEW_PRIVS` (Linux) |
| **Opt-in per-function caps** | `memory_mb` → `RLIMIT_AS` · `cpu_time_secs` → `RLIMIT_CPU` · `allowed_paths` → Linux Landlock fs allowlist |
| **WASI sandbox** | `runtime = "wasm"` — deny-by-default filesystem + network; capabilities granted explicitly |

---

## Observability

| Endpoint / Feature | Detail |
|---|---|
| `/_riz/health` | Rich health check (open, no auth) |
| `/_riz/metrics` | Prometheus-compatible scrape endpoint |
| `/_riz/registry` | Live function registry |
| **Terminal dashboard** | `riz run --dev` — Ratatui TUI with P50 / P75 / P90 / P95 / P99 latency over 5-min rolling window |
| **Datadog** | Metrics emitter — works with Datadog Agent's OpenMetrics integration |
| **OpenTelemetry** | *coming (v0.2)* — single-path OTEL exporter + W3C Trace Context + token-aware tracing |

---

## Performance Headline

> **91,419 req/s · p99 845 µs** — Bun `ping` handler, localhost, concurrency=20, M-series Mac.

This measures the riz dispatch path (routing + process pool bridge). Real throughput is bounded by handler code. Methodology and caveats: [`benches/README.md`](../benches/README.md).

---

## Developer Experience

| Command | What it does |
|---|---|
| `riz init <template>` | Scaffold a working project — 7 templates: `typescript-http` · `nodejs-http` · `python-http` · `rust-http` · `typescript-websocket` · `python-websocket` · `rust-websocket` |
| `riz run` | Headless (JSON logs to stdout); `--dev` adds the live TUI |
| `riz run --dev` | Boots the Ratatui terminal dashboard with hot-reload |
| `riz validate` | Config check — parse + validate `riz.toml` |
| `riz routes` | Print the full route table |
| `riz mcp inspect` | `initialize` + `tools/list` one-screen report |
| `riz deploy` | S3 hot-swap — in-flight requests drain over 30 s, new pool promoted atomically, auto health-check rollback |
| `riz doctor` | Preflight environment check |

---

## Proof

| Metric | Value |
|---|---|
| **Test count** | 778 tests (`cargo nextest run`, ~60 s) |
| **Parity matrix** | Every HTTP capability tested identically across all 5 runtimes |
| **Bug tracker** | 20 / 20 production-readiness entries closed — each with a regression-gate test |

---

## Roadmap (v0.2 — coming)

| Item | Summary |
|---|---|
| **WASM guards** | Pre/post-invoke `.wasm` guards — validate schemas, redact PII, scrub secrets, enforce response shape across every runtime |
| **Gateway: Bedrock + Vertex** | Additional providers to the shipped OpenAI / Anthropic / Ollama routing |
| **Semantic cache** | Similarity-based cache — targets 30–70% cost reduction on repetitive workloads |
| **Record & replay** | `riz replay --since 1h` — diff handler responses against captured traffic; dataset export for fine-tuning |
| **Eval harness** | `riz eval <function>` — rank prompt × model × guard combos on quality / cost / latency |
| **Smarter MCP** | Per-route typed schemas auto-derived from TS / Python / Rust types; MCP over WebSocket; OAuth 2.1; federation |
| **Go runtime** | Go support via static-binary protocol (thin `riz-go-runtime` module + templates) |
| **OpenTelemetry** | Single-path OTEL exporter, W3C Trace Context, token-aware tracing |
| **Non-HTTP event sources** | SQS / SNS / S3 / EventBridge triggers |

---

*Last updated: 2026-06-09*
