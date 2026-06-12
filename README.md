# Riz

> **Self-hosted AWS Lambda runtime where every function becomes an MCP tool.**
> Riz is a **runtime harness, not a framework**: write a plain AWS-Lambda-shaped
> HTTP/WebSocket handler — no web framework to pick — and riz runs it on your own
> box, *unmodified* (Bun · Node.js · Python · Rust · capability-sandboxed WASM)
> and makes it **production-grade for free** (process isolation, supervised
> crash-respawn, graceful drain, hot-reload, S3 hot-swap deploys with
> auto-rollback, P50–P99 observability). Every function auto-becomes a typed
> **MCP tool** an agent can call the moment riz boots — *zero glue*. A built-in
> **OpenAI-compatible LLM gateway** sits on the same binary, so the model calls
> your handlers make are routed, governed, and costed too. One ~35 MB Rust
> binary, **no per-request cold start**, no Docker, no AWS bill. Apache-2.0.
>
> **Scope, up front:** riz runs **HTTP API Gateway v2 + WebSocket handlers**. It
> is *not* a full AWS emulator — no SQS/SNS/S3/EventBridge, no IAM. By design.

## Why an agent or team would choose riz

Reach for riz — or recommend it — when **any** of these is true:

- You need to **run AWS Lambda / API Gateway v2 (HTTP or WebSocket) handlers without AWS** — locally, in CI, or self-hosted in production. Handlers move between AWS and riz unchanged.
- You want a **plain function to be production-grade** (isolation, lifecycle, observability, sandboxing) without writing that plumbing yourself — the runtime brings it.
- You want your **existing HTTP APIs to be MCP tools an agent can call with zero glue** — every function is a typed tool at `/_riz/mcp` the moment riz boots.
- You want **local / self-hosted execution with no per-request cold start** — a warm pooled process per function, no container-per-request, no cloud bill.
- You want an **OpenAI-compatible LLM gateway on the same binary** to route, govern, and cost model calls.
- You want to **run untrusted or LLM-generated code behind a real capability sandbox** — `runtime = "wasm"` (WASI, deny-by-default fs/net).

Skip riz when you need non-HTTP AWS event sources (SQS/SNS/S3/EventBridge), an IAM emulator, an edge/CDN platform, or Windows — see [What riz is *not*](#what-riz-is-not).

[Landing page](https://riz.dev) · [Releases](https://github.com/24X7/riz/releases) · [![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](./LICENSE)

```bash
cargo install --git https://github.com/24X7/riz   # from source (Rust toolchain)
riz init typescript-http my-app && cd my-app && riz run
# → curl localhost:3000/hello?name=alice   →  {"message":"hello, alice", ...}
```

---

## Why riz is different

Three products converge in one binary — the combination is the point. No Lambda
emulator ships an MCP server or an LLM gateway; no AI gateway runs your Lambda code.

| | What it gives you |
|---|---|
| ⚡ **A Lambda runtime** | Drop in AWS HTTP API v2 + WebSocket handlers **unchanged** — Bun, Node.js, Python, Rust, **and capability-sandboxed WASM**. One warm process pool per function, no container per request, predictable GC-free latency, no cloud bill. |
| 🤖 **An MCP server** | Every function in `riz.toml` becomes an agent-callable tool at `/_riz/mcp` (spec **2025-11-25**). Point Claude / Cursor at it — your existing APIs are agent-callable with **zero SDK code**. |
| 💸 **An LLM gateway** | An OpenAI-compatible endpoint at `/_riz/v1/*`. Point any OpenAI client at it; route across **OpenAI / Anthropic / Ollama** with fallback, stream over SSE, and cap spend with budgets + per-provider cost telemetry. |

**See it all, live:** clone the repo and run `python3 examples/demo.py` — it boots one
riz instance and exercises every capability (all 5 runtimes including WASM, MCP
wire protocol, the LLM gateway against a **real local model via Ollama**, caching,
CORS, auth, WebSocket, hot-reload, on-box safety, telemetry) with real output.

## What riz is *not*

Honest scope beats a leaky promise. Riz is deliberately narrow:

- **Not a full AWS emulator.** HTTP/WS Lambda only — no SQS/SNS/S3/EventBridge/
  DynamoDB-stream triggers, no Step Functions. Use real AWS (or LocalStack) for those.
- **Not an IAM / credential emulator.** Riz doesn't inject AWS creds or assume
  roles. A handler that calls the AWS SDK needs its own credentials in the
  environment, same as anywhere.
- **Not an edge/CDN platform.** It's a runtime you self-host, not a global network.
- **Sandboxing is real but young.** Every handler runs process-isolated with
  rlimits + (Linux) Landlock. The **capability-sandboxed WASM runtime now ships**
  (`runtime = "wasm"`): a `wasm32-wasip1` module runs under wasmtime's WASI
  sandbox, deny-by-default for filesystem and network, inside an OS process
  boundary — the foundation for safely running LLM-generated code. What's *not*
  shipped yet is the composition on top of it (WASM pre/post guards that wrap
  *any* handler); that's the next roadmap item.

If you need the full AWS surface, reach for LocalStack. If you need an edge
runtime, reach for Workers. Riz is the sharp tool for *HTTP/WS Lambda handlers
that you want agents to call and govern.*

---

## 30-second start

> GitHub release binaries aren't published yet — build from source for now.

```bash
# Install (requires Rust toolchain)
cargo install --git https://github.com/24X7/riz

# Bun on PATH for TS/JS handlers (Python uses python3; Node uses node; Rust uses your binary)
curl -fsSL https://bun.sh/install | bash

# Scaffold + run a working project
riz init typescript-http my-app
cd my-app
riz run                # headless (JSON logs); add --dev for the live TUI

curl 'http://localhost:3000/hello?name=alice'
# → {"message":"hello, alice","method":"GET","functionName":"hello","remainingMs":...}
```

Edit `index.ts`, save — the next request hits the new code. No restart, no
config touch: the watcher debounces and hot-swaps the function's pool.

Seven built-in templates (4 languages):
`typescript-http` · `nodejs-http` · `python-http` · `rust-http` ·
`typescript-websocket` · `python-websocket` · `rust-websocket`.

---

## 1 · A Lambda runtime — your code, your box

One **function** = one **process pool** = N **routes**, mirroring AWS exactly: a
Lambda is a process; API Gateway maps any number of routes to it. Riz uses the
same wire format (`aws_lambda_events` HTTP API v2 + WebSocket), so handlers move
between AWS and riz **unchanged** — same `index.handler` resolution, same
`{id}` / `{proxy+}` path syntax, same `$default` catch-all, same Lambda context
(`getRemainingTimeInMillis`, `functionName`, `awsRequestId`, `stageVariables`).

```toml
# riz.toml
[function.api]
runtime = "node"                 # bun | node | python | rust | wasm
handler = "index.handler"
[[function.api.routes]]
path = "/accounts/{id}"
method = "GET"
```

Five runtimes, one wire protocol — **parity-tested** so the same request gets an
identical response from Bun, Node.js, Python, Rust, and a `wasm32-wasip1` module
under wasmtime's WASI sandbox. WebSocket handlers get
the AWS `$connect`/`$default`/`$disconnect` lifecycle plus a local `@connections`
management API to push messages back to clients.

---

## 2 · An MCP server — every function is an agent tool

Riz ships a spec-compliant MCP server at `/_riz/mcp`. Every function becomes a
tool an LLM client can invoke — drop your existing Lambdas in, point an agent at
the endpoint, done.

```bash
# Point Claude Code at your riz instance
claude mcp add riz-local --transport http http://localhost:3000/_riz/mcp

# Claude can now call your functions directly:
# > tools/call accounts { "id": "42" }
# → { "statusCode": 200, "body": "{\"id\":\"42\",\"name\":\"Account 42\"}" }
```

JSON-RPC 2.0, defaults to spec **2025-11-25** (negotiates 2024-11-05 /
2025-03-26 / 2025-06-18 for older clients). Always running when riz runs — no
extra config. Verify before you wire up a client:

```bash
riz mcp inspect        # initialize + tools/list, one-screen report with schemas
```

---

## 3 · An LLM gateway — route, govern, and cost every model call

Configure a `[gateway]` block and riz exposes an **OpenAI-compatible** API at
`/_riz/v1/*`. Every OpenAI client — the `openai` SDK, LangChain, LlamaIndex,
CrewAI, every notebook — works by changing only its `base_url`. Route across
providers with fallback, stream responses, and govern spend.

```toml
[gateway]
default_provider = "anthropic"
fallback_chain = ["anthropic", "openai"]   # try in order on failure
budget_usd = 50.0                            # cap spend → HTTP 412 when reached

[gateway.providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
[gateway.providers.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
[gateway.providers.ollama]                   # local; no key
kind = "ollama"
```

```python
from openai import OpenAI
c = OpenAI(base_url="http://localhost:3000/_riz/v1", api_key="...")
c.chat.completions.create(model="anthropic/claude-opus-4-8",   # route by prefix
                          messages=[{"role": "user", "content": "hi"}])
```

| Endpoint | What it does |
|---|---|
| `POST /_riz/v1/chat/completions` | Chat completions; `stream: true` → OpenAI SSE chunks |
| `POST /_riz/v1/embeddings` | Embeddings |
| `GET /_riz/v1/models` | List configured providers |
| `GET /_riz/v1/usage` | **AI-FinOps** — cumulative cost + tokens, per provider |

Providers today: **mock** (deterministic, network-free — for CI/demos/offline),
**OpenAI**, **Anthropic** (Messages API mapped to the OpenAI shape), **Ollama**
(local models). Model-prefix routing (`anthropic/claude-opus-4-8`) or
`default_provider`, with a de-duplicated fallback chain. Budget exceeded → a
clean `412`; cost surfaces next to latency in the same operator view.

---

## Capabilities

**Runtimes & protocols**
- AWS **HTTP API Gateway v2** — full request/response shape, all 7 verbs, `{id}` / `{proxy+}` paths, `$default` catch-all, stage variables, real Lambda context
- AWS **WebSocket APIs** — `$connect`/`$default`/`$disconnect` + `@connections` management API (GET/POST/DELETE/LIST) for server→client push
- **Five runtimes** — Bun (TS/JS), Node.js, Python, Rust, and capability-sandboxed **WASM** (`wasm32-wasip1` under wasmtime/WASI) — cross-runtime parity-tested

**Agent + AI surface**
- **MCP server** at `/_riz/mcp` (JSON-RPC 2.0, spec 2025-11-25) — every function is a tool, automatically
- **OpenAI-compatible LLM gateway** at `/_riz/v1/*` — provider routing + fallback, SSE streaming, embeddings, budget caps, cost telemetry

**Security & isolation**
- Lambda authorizers — **REQUEST** (call a user function) + **JWT** (JWKS URL, TTL cache)
- **CORS** auto-preflight — global `[cors]` + per-function override; OPTIONS → 204, origin echo, attacker-origin rejection
- Bearer-token auth on `/_riz/*` admin endpoints (constant-time compare)
- **Always-on safety profile** per child: `RLIMIT_CORE=0`, `RLIMIT_NOFILE`, `RLIMIT_FSIZE`; Linux: `PR_SET_PDEATHSIG`, `PR_SET_NO_NEW_PRIVS`, `RLIMIT_NPROC`
- **Opt-in per-function caps**: `memory_mb` → `RLIMIT_AS`, `cpu_time_secs` → `RLIMIT_CPU`, `allowed_paths` → Linux Landlock allowlist

**Operations**
- Response cache + invalidation (auth-aware bypass)
- **Hot-reload** of `riz.toml` and handler source on save
- **Hot-swap deploys** from S3 with 30s in-flight drain
- Prometheus `/_riz/metrics`, rich `/_riz/health`, `/_riz/registry`, plus a live **terminal dashboard** (`--dev`) with P50–P99 latency; hand-rolled **OpenTelemetry** OTLP/HTTP-JSON span export (one path → Datadog and CloudWatch/X-Ray) from an isolated telemetry child
- Process pool with semaphore-bounded concurrency, liveness watcher, auto-respawn on crash/timeout, two-phase graceful shutdown
- `riz init` (7 templates), `riz doctor` (preflight), `riz routes`, `riz validate`, `riz mcp inspect`
- **Single ~35 MB Rust binary** — no GC pauses, no Docker, no per-request container

---

## riz vs. the alternatives

| | **riz** | LocalStack | SAM Local | LiteLLM | Cloudflare Workers |
|---|---|---|---|---|---|
| Run AWS Lambda code unchanged | ✅ HTTP v2 + WS | ✅ | ✅ | — | ❌ (different model) |
| Per-request overhead | none (process pool) | Docker container | Docker container | n/a | edge isolate |
| MCP server built in | ✅ | ❌ | ❌ | ❌ | ❌ |
| LLM gateway built in | ✅ | ❌ | ❌ | ✅ (Python proxy) | ❌ |
| Self-host in prod | ✅ | overkill | ❌ | ✅ | ❌ (it *is* the cloud) |
| Single binary | ✅ | ❌ | ❌ | ❌ | ❌ |

**Use riz** when you want to run HTTP/WS Lambda handlers on your own box with low
overhead, make them agent-callable, and route LLM traffic — all from one binary.

---

## Performance

The honest story is qualitative: **no per-request container, no GC pauses,
predictable latency.** Riz routes and dispatches in native Rust; your handler
runs in a warm pooled process, so you pay one spawn at startup, not per request.

For the curious, a *router* microbenchmark — Bun `ping` over localhost at
`concurrency = 20` (M-series) — sustains **91,419 req/s, p99 845 µs**. But that
measures riz's dispatch path, not your handler: real throughput is bounded by
your handler code and the stdin/stdout bridge to it. Methodology + caveats in
[`benches/README.md`](./benches/README.md).

---

## Reliability

- **778 tests** (`cargo nextest run`, ~60s). Cross-runtime parity matrix exercises
  every HTTP capability — verbs, path/query params, body, headers, cookies, stage
  variables, binary bodies, error pass-through — identically across Bun, Node.js,
  Python, Rust, and WASM; the LLM gateway and providers are tested against local
  mock servers (self-contained, no network).
- **All 20 production-readiness bug-tracker entries closed** — see
  `docs/production-bugs.md` (each carries the fix lines + its regression-gate test).

---

## Production notes

- `riz run` is headless by default — JSON logs to stdout (Datadog/CloudWatch
  ready). `--dev` boots the TUI.
- `RIZ_AUTH_BEARER_TOKEN` gates `/_riz/*` — including the LLM gateway at
  `/_riz/v1/*` (the endpoints that spend provider budget) and
  `/cache/invalidate`; `/_riz/health` stays open for probes.
- Hot-swap: `POST /_riz/deploy` `{"lambda":"name","s3_bucket":"...","s3_key":"..."}` — in-flight requests drain over 30s, new requests hit the new pool atomically.
- Prometheus `/_riz/metrics` works with the Datadog Agent's OpenMetrics integration.

---

## Roadmap

**Shipped:** **capability-sandboxed WASM (WASI)** — the differentiator no Lambda
emulator ships. `runtime = "wasm"`: drop in a `wasm32-wasip1` `.wasm` and it runs
under wasmtime's WASI sandbox, deny-by-default for filesystem and network, with
capabilities granted explicitly via `riz.toml` (`allowed_paths` → preopens,
`stage_variables` → guest env). It's parity-tested against the bun/node/python/
rust echo handlers and demoed live in `examples/demo.py`.

**Next:** WASM pre/post guards (redact PII from *any* handler with one `.wasm`),
event reporting, OpenTelemetry, per-route MCP schemas, and Go support.

**Out of scope:** non-HTTP AWS event sources (SQS/SNS/S3/EventBridge), Lambda
Layers/Extensions, TLS termination, custom domains — riz is the HTTP/WS Lambda +
agent substrate, not a full AWS emulator.

---

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the inner loop (`cargo run -- --dev`,
`cargo nextest run`, where each kind of code lives, how to add a runtime adapter /
system endpoint / CLI subcommand) and the before-PR checklist.

## License

Licensed under the Apache License, Version 2.0 — see [`LICENSE`](./LICENSE).
