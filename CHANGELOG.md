# Changelog

All notable changes to riz are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and riz aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 - 2026-06-29

First public release. A self-hosted AWS Lambda + API Gateway v2 runtime in one
Rust binary, where every function is also an agent's tool.

### Added

- **Lambda runtime** — runs AWS HTTP API Gateway v2 **and** WebSocket handlers
  unmodified (`aws_lambda_events` wire shape): `index.handler` resolution,
  `{id}`/`{proxy+}` paths, `$default`, real Lambda context. One
  `[function.<name>]` = one warm process pool = N routes. No per-request cold
  start.
- **Five runtimes**, parity-tested: Bun, Node.js, Python, Rust, and
  capability-sandboxed **WASM** (`wasm32-wasip1` under wasmtime/WASI).
- **MCP server** at `/_riz/mcp` — every function is a typed MCP tool
  (JSON-RPC 2.0 over Streamable HTTP, spec 2025-11-25 with negotiation), typed
  per-route schemas, SSE transport, and progress notifications.
- **OpenAI-compatible LLM gateway** at `/_riz/v1/*` — OpenAI / Anthropic /
  Ollama / mock providers, model-prefix routing + fallback, SSE streaming,
  embeddings, budget caps (HTTP 412), and per-provider cost telemetry.
- **WASI capability broker** — sandboxed WASM can query Postgres host-side
  through a `[function.x.capabilities]` grant (no sockets/DSNs in guest memory;
  deadlines, rate limits, payload caps). **WASM guards** (`guard_in`/`guard_out`)
  run a policy module on every request/response across all runtimes, fail-closed.
- **Static file serving** (`[static]`) — colocate an SPA/site on the same binary
  and origin as the API (no CORS); traversal/symlink/dotfile-safe; ETag/304,
  Range/206, hash-named immutable caching, SPA fallback. A live instance can
  serve its own `llms.txt` + `.well-known/riz.json`.
- **`riz scaffold static`** — generate the agent-discovery files from your
  functions. **`riz init`** fetches templates from any git location (official
  names, `owner/repo[/subdir]`, git URL, or local path — never embedded),
  including a full-stack `typescript-todo` example.
- **Security & isolation** — always-on per-child safety profile (rlimits,
  `PR_SET_PDEATHSIG`, `PR_SET_NO_NEW_PRIVS`), opt-in `memory_mb`/`cpu_time_secs`/
  Landlock `allowed_paths`; JWT/JWKS + REQUEST authorizers; CORS; bearer-gated
  `/_riz/*`.
- **Operations** — response cache, hot-reload, S3 hot-swap deploys with 30s
  drain + health-check auto-rollback, Prometheus `/_riz/metrics`, OpenTelemetry
  OTLP/HTTP trace export (Datadog/Honeycomb/Tempo/Jaeger/X-Ray via a collector;
  current OTel GenAI token attributes), and a live `--dev` terminal dashboard.
- **Claims-as-code** — every capability claim on the website is pinned to a
  passing test (`tests/claims/registry.toml`).
