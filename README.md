# Riz

> **Self-hosted AWS Lambda runtime.** HTTP API Gateway v2 + WebSocket APIs compatible. Single Rust binary. Bun, Python, and Rust handlers. No Docker, no AWS bill.

[Landing page](https://riz.dev) · [Releases](https://github.com/crizzuto/riz/releases) · MIT licensed

## 30-second start

> **Note:** GitHub release binaries are not yet published. Use the source build path below until a v0.1.0 release tag exists on GitHub.

```bash
# Install from source (requires Rust toolchain)
cargo install --git https://github.com/crizzuto/riz

# Binary install (macOS / Linux) — works once release binaries are published
# curl -fsSL https://riz.dev/install | sh
```

Bun must be on PATH for TypeScript/JavaScript handlers (Python uses
`python3`; Rust uses your prebuilt binary directly):

```bash
curl -fsSL https://bun.sh/install | bash
```

Scaffold a working project in one command:

```bash
riz init typescript-http my-app
cd my-app
riz run
# → curl localhost:3000/hello?name=alice
#   {"message":"hello, alice","method":"GET", ...}
```

Other templates (3 languages × 2 scenarios):
`typescript-http` · `python-http` · `rust-http` ·
`typescript-websocket` · `python-websocket` · `rust-websocket`.

Edit `index.ts`, save, the next request hits the new code — no
restart, no `riz.toml` touch. The watcher debounces and hot-swaps
the function's pool automatically.


## Mental model

One **function** = one **process pool** = N **routes**. The mapping matches AWS: a Lambda is a process. A process serves any number of routes via API Gateway. Riz uses the same wire format (`aws_lambda_events` HTTP API v2 + WebSocket API), so handlers move between AWS and riz unchanged.

**Handler resolution** — `handler = "index.handler"` splits on the last dot: file `index.ts`, export `handler`. Any export name works. The explicit path form `"./src/api/index.ts"` also works but the dot-separated form is the AWS convention.

**`$default`** — HTTP catch-all route key. Add `path = "/{proxy+}" method = "ANY"` to a routes block, or omit routes entirely and riz mounts the function at `/$name` with `ANY` method.

**`{proxy+}`** — AWS greedy path capture. Matches `/api/anything/here` and populates `event.pathParameters.proxy`.

**WebSocket routes** — `$connect`, `$default`, `$disconnect` are the three magic route keys. Set `protocol = "websocket"` on the function block. Route selection for `$default` is based on the request body field specified in `route_selection_expression`.

**MCP tools** — every function in `riz.toml` is automatically exposed as an MCP tool at `/_riz/mcp`. Route parameters become tool input fields. The handler receives a real HTTP API v2 event with the parameters filled in.

## Why MCP matters

Riz ships a spec-compliant MCP server at `/_riz/mcp`. Every function in your `riz.toml` automatically becomes a tool an LLM client can invoke. Drop your existing Lambdas in, point Claude or Cursor at `http://localhost:3000/_riz/mcp`, and your APIs are agent-callable with zero SDK code.

```bash
# Point Claude at your riz instance
claude mcp add riz-local --transport http http://localhost:3000/_riz/mcp

# Claude can now call your functions directly:
# > tools/call api { "id": "42" }
# → { "statusCode": 200, "body": "{\"id\":\"42\",\"name\":\"Account 42\"}" }
```

The MCP server speaks JSON-RPC 2.0 and defaults to spec **2025-11-25** (current stable). It still negotiates the older 2024-11-05, 2025-03-26, and 2025-06-18 baselines for clients that haven't moved yet — older clients get their requested version echoed back on `initialize`. Supported lifecycle methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/templates/list`, `prompts/list`. No extra config — it's always running when riz runs.

Note on batching: JSON-RPC batch requests were removed in MCP 2025-06-18. Riz still accepts batches when a 2024-11-05 or 2025-03-26 client sends them; new clients targeting 2025-06-18+ should send single requests.

Bearer-token protection: set `RIZ_AUTH_BEARER_TOKEN` or `[auth] bearer_token` in `riz.toml` to require `Authorization: Bearer <token>` on `/_riz/mcp` and other system endpoints. `/_riz/health` stays open for liveness probes.

**Verify your setup** before pointing Claude or Cursor at the endpoint:

```bash
riz mcp inspect
```

Runs `initialize` + `tools/list` against your running Riz and prints a one-screen report — spec version, server capabilities, every registered tool with its input + output schemas. Add `--bearer <token>` (or set `RIZ_AUTH_BEARER_TOKEN`) for auth-gated endpoints; `--url` to point at a remote instance.

**Troubleshoot "it won't start":**

```bash
riz doctor
```

Pre-flight diagnostic. Validates `riz.toml`, checks runtime binaries (`bun` / `python3`) for every runtime your config uses, confirms each function's handler file exists, probes the configured port. Prints ✓ / ⚠ / ✗ per check + a summary. Designed to be the first command you run when something's off, before opening an issue.

## Features (v0.1)

**Shipping today:**
- AWS HTTP API Gateway v2 — full request/response shape, all 7 verbs (cross-runtime parity-tested: TS/JS via Bun, Python, Rust)
- AWS WebSocket APIs — `$connect` / `$default` / `$disconnect` + `@connections` management API at `/_riz/connections/{id}` (GET/POST/DELETE) and `/_riz/connections` (LIST). Handlers in **Bun, Python, and Rust** (all three end-to-end tested).
- Bun, Python, and Rust runtime adapters
- Lambda context — `getRemainingTimeInMillis`, `functionName`, `invokedFunctionArn`, `awsRequestId`
- Lambda authorizers — REQUEST (verified end-to-end with Bun) + JWT (with JWKS URL, TTL cache)
- CORS auto-preflight — `[cors]` config block, OPTIONS → 204, echoed `Access-Control-Allow-Origin` on non-preflight, attacker-origin rejection
- Bearer-token auth on `/_riz/*` system endpoints
- Hot-swap deploys from S3 with in-flight request drain
- `riz.toml` hot-reload on save
- `/_riz/health` · `/_riz/metrics` · `/_riz/registry` · `/_riz/mcp` · `/_riz/connections` · `/_riz/connections/{id}`
- Terminal dashboard with P50–P99 latency, process stats, log stream
- Datadog metrics emitter
- Single Rust binary, ~10 MB
- **On-box safety profile** (every spawned child, no opt-in needed): `RLIMIT_CORE=0` (no core-dump disk fill), `RLIMIT_NOFILE=4096` (FD-leak cap), `RLIMIT_FSIZE=100MiB` (write cap). Linux only: `PR_SET_PDEATHSIG(SIGKILL)` (orphan prevention), `PR_SET_NO_NEW_PRIVS` (privilege downgrade), `RLIMIT_NPROC=256` (fork-bomb cap).
- **Opt-in per-function caps**: `memory_mb` → `RLIMIT_AS` (AWS Lambda's `MemorySize`), `cpu_time_secs` → `RLIMIT_CPU` (kills runaway loops), `allowed_paths` → Linux Landlock filesystem allowlist (kernel 5.13+)

## Roadmap (v0.2 and beyond)

**Capability-sandboxed WASM (WASI)** — the differentiator no Lambda emulator ships: WebAssembly handlers wrapped in a host process that grants filesystem, network, and clock capabilities explicitly via `riz.toml`. Same line-delimited JSON protocol as the Bun/Python/Rust adapters; the WASM runtime (wasmtime/wasmer) enforces the capability boundary. Sub-millisecond cold start, one `.wasm` binary across Linux/macOS/edge. Targets the multi-tenant SaaS + untrusted-MCP-tools use cases.

**Additional runtimes**
- Node.js native runtime — for shops that won't ship Bun in prod
- Go support via the existing static-binary protocol (thin `riz-go-runtime` module + templates + examples; the runtime kernel is the same one Rust uses)
- Java / JVM runtime adapter

**Smarter MCP**
- Per-route MCP tool schemas — typed input shapes from path + query parameters
- AI inspection tools — `riz.tail_logs`, `riz.replay_request`, `riz.scaffold`
- OAuth 2.1 + RFC 8707 Resource Indicators (bearer-token path stays the default)

**Operability**
- OpenTelemetry exporter with W3C Trace Context (X-Ray header propagation comes free)
- Non-HTTP event sources (SQS, SNS, S3, EventBridge, scheduled)

**Out of scope:**
- Lambda Layers + Extensions — vendor deps belong in the handler dir
- Custom domain mappings — reverse-proxy concern
- TLS termination — terminate Let's Encrypt at the edge (Caddy/nginx)

## vs. LocalStack / SAM Local / Cloudflare Workers

| | Riz | LocalStack | SAM Local | Workers |
|---|---|---|---|---|
| Surface | HTTP API v2 + WS Lambda only | Full AWS emulation | Lambda + API Gateway | Workers runtime |
| Per-request cost | None (process pool) | Docker container | Docker container | Edge compute |
| Cold start | ~50ms (process spawn) | seconds (Docker) | seconds (Docker) | ~5ms (V8 isolate) |
| Local dev UX | Live TUI with P50-P99 | None | None | wrangler dev |
| MCP server | Built-in | No | No | No |
| AWS Lambda code unchanged | Yes (HTTP API v2) | Yes | Yes | No (different model) |
| Self-host in prod | Yes | Possible (overkill) | No | No (it IS the cloud) |
| Single binary | Yes | No | No | No |

When to use what:
- **Riz** — you want to run HTTP/WS Lambda handlers on your own box with low overhead, want the terminal dashboard, want MCP integration.
- **LocalStack** — you need the full AWS emulation surface for local development of multi-service apps.
- **SAM Local** — you're already deep in CloudFormation and want AWS-tooling-compatible local invocation.
- **Cloudflare Workers** — you want edge compute, willing to write to the Workers API instead of Lambda's.

## Examples

See `examples/lambdas/`:

**HTTP handlers**
- `ping` (Bun) — bare-minimum, returns `{ status: "ok", ts }`. No routes block → mounts at `ANY /ping`.
- `accounts` (Bun) — REST GET with `{id}` path param, demonstrates `event.pathParameters` + `rawQueryString` parsing.
- `events` (Bun) — POST endpoint that validates and echoes a JSON body.
- `crud-accounts` (Bun) — full CRUD (GET/POST/PUT/PATCH/DELETE) on `/accounts/{id}`, demonstrates all HTTP verbs + `method = "ANY"`.
- `echo-bun` / `echo-python` / `echo-rust` — minimal echo handlers, one per shipped runtime. Used by the cross-runtime parity test suite.

**WebSocket handlers** (all three runtimes)
- `chat` (Bun) — `$connect` / `$default` / `$disconnect`. Echoes via the `@connections` API.
- `chat-python` — same shape, Python stdlib `urllib.request` for the `@connections` POST.
- `chat-rust` — same shape, `reqwest` (no-TLS) for the `@connections` POST.

Run any example:

```bash
riz run --config examples/riz.dev.toml
```

Or scaffold a fresh project from any of the 6 built-in templates with `riz init <template> <dir>` (see [30-second start](#30-second-start)).

## Reliability

- **All 20 production-readiness bug-tracker entries closed.** See `docs/production-bugs.md` — every entry carries a `✅ RESOLVED` marker with the code lines that ship the fix and the regression-gate test name.
- **680+ tests, drift-prevented landing page.** `cargo nextest run` runs the full suite. `tests/landing_page_contract.rs` enforces every claim on this README and the landing page against a real proof test — removing a feature without removing its claim fails CI.
- **Cross-runtime parity-tested.** Each shipped runtime (Bun, Python, Rust) is exercised end-to-end through the same matrix of HTTP capability tests (status codes, verbs, path params, query string, body, headers, cookies, stage variables, binary body, error pass-through, response headers, response cookies). WebSocket lifecycle + `@connections` is also end-to-end tested per runtime.

## Production

- `riz run --no-tui --log-level info` runs in headless mode with JSON logs (structured for Datadog/CloudWatch ingestion).
- Set `RIZ_AUTH_BEARER_TOKEN` to gate `/_riz/*` admin endpoints with a shared secret. `/_riz/health` stays open for liveness probes.
- Hot-swap a function by POSTing to `/_riz/deploy` with `{"lambda": "name", "s3_bucket": "...", "s3_key": "..."}`. In-flight requests drain over 30 seconds; new requests hit the new pool atomically.
- The Prometheus metrics at `/_riz/metrics` are compatible with Datadog Agent's OpenMetrics integration and direct scraping.

## License

MIT.
