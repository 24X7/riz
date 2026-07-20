# Changelog

All notable changes to riz are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and riz aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Changed

- `riz init` is now `riz new`. The scaffold set is six per-runtime templates —
  `typescript-bun`, `typescript-node` (real TypeScript on Node's native type
  stripping, node >= 22.18), `python`, `rust`, `go`, `wasm-rust` (authored on
  the new `riz-wasm` shim: you write a Lambda handler, the shim owns the wire)
  — plus the `typescript-todo` / `ai-chat` example starters. The three
  WebSocket templates are gone; `examples/chat` is the WS showcase.
- WASM guests are authored as pure Lambda handlers on the `riz-wasm` crate;
  hand-written stdin loops are banned from examples and templates by a
  conformance test, and `tests/template_smoke_all.rs` scaffolds, builds, and
  boots every template as an isolated smoke suite.

### Added

- **End-to-end validation flow.** `scripts/validate.sh` runs the whole
  proof-of-life in one command against the built binary: the example fleet
  (all six runtimes + control plane), the `riz new` scaffold journey, a
  performance floor + trend (HTTP and brokered-capability throughput/latency),
  and a chaos suite that deliberately injects failures — pool saturation,
  worker SIGKILL, the crash-loop circuit breaker, broker backend loss, and
  SIGTERM drain — and asserts riz survives with no orphaned processes. Added
  as `tests/perf_regression.rs` and `tests/chaos.rs` (isolated CI steps).

- **`http` brokered capability.** A WASM guest can now reach an outbound HTTP
  origin through the broker: `[resources.http.<name>]` pins the origin
  (`base_url`) and the daemon injects auth host-side, so the guest names a
  grant and supplies a relative path — never a credential or an absolute URL.
  SSRF-hardened by default: redirects are not followed, and a host that
  resolves into loopback/private/link-local space is refused (opt out per
  origin with `allow_private_ips` for an operator-declared internal service).
  Per-grant `methods` allow-list; `mode = "read-only"` forces `GET`. The guest
  API is `riz_wasm::cap::http::fetch`.

### Security

- Workers no longer inherit the daemon's full environment. Every worker is
  spawned with `env_clear()` plus a conservative allowlist (PATH, HOME, locale,
  TLS-root and proxy vars, and riz's own non-secret control vars), so a
  resource DSN or any other daemon secret lives in exactly one process — the
  daemon. A function's own `[function.X.env]` remains the documented escape
  hatch. Enforced by a secrets-canary test.

### Fixed

- Lambda proxy responses with `null` or missing `cookies` / `headers` /
  `multiValueHeaders` / `isBase64Encoded` are now accepted exactly like real
  AWS (stock `aws-lambda-go` handlers marshal nil slices as `null` and
  previously drew a 502 "bad gateway").

## 0.2.2 - 2026-07-18

### Fixed

- `riz --dev` no longer leaves the terminal broken (stuck in raw mode + mouse
  capture, spewing escape codes) when startup fails because the port is already
  in use. The listener is now bound **before** the TUI takes over the terminal,
  so a port-in-use (or any bind) failure prints the actionable error on a normal
  terminal instead of after the console has been put into raw mode. If your
  terminal is ever left in this state by an older build, `reset` restores it.

## 0.2.1 - 2026-07-15

### Fixed

- `riz init <template>` with no target directory now scaffolds into a **new
  directory named after the template** (like `cargo new` / `git clone`), not
  the current directory. `riz init ai-chat && cd ai-chat` works as expected;
  the previous behavior failed in any non-empty directory and, when it did
  succeed, left nothing to `cd` into. Pass `.` as the directory to scaffold in
  place. The dir name is derived from the spec's basename, so
  `owner/repo/tmpl`, git URLs, and local paths all get a sensible default too.

## 0.2.0 - 2026-07-13

Agents, edge controls, and a safety-critical rewrite of the whole binary. riz
is now an agent2agent-protocol server in its own right, gates the data plane
per caller, and every line compiled into the binary is held to NASA's Power of
10 rules.

### Added

- **A2A built-in agent** — set `[agent]` and this instance becomes an
  agent2agent-protocol server: an Agent Card at
  `/.well-known/agent-card.json` and a JSON-RPC endpoint at `/_riz/a2a` where
  peers delegate tasks. It reasons through the LLM gateway with this instance's
  own functions as tools, streams live task events over SSE
  (`SendStreamingMessage`), and forms a mesh via `[agent.peers]` with
  hop-capped delegation (`riz a2a send`).
- **Per-caller API keys + token-bucket rate limiting** — `[api_keys.<name>]`
  maps a caller to a secret + rate ceiling. Non-`/_riz/*` requests (function
  invocations, WebSocket handshakes, colocated static) must present a matching
  `X-Api-Key`; unknown/absent keys fail closed (401) and each caller has its
  own bucket (429 + `Retry-After`). No keys → open, unchanged.
- **Structured audit log** — deploy, config-reload, and auth-denial events on
  the `riz.audit` tracing target, scrubbed of secret material; route them with
  `RUST_LOG=riz.audit=info`.
- **Production metrics** — the four golden signals plus worker supervision and
  cache efficiency at `/_riz/metrics`, including saturation
  (`riz_concurrency_in_use`/`_limit`, `riz_admission_rejected_total`) and a
  cross-instance-aggregatable `riz_request_duration_seconds` histogram. A
  `/_riz/ready` readiness probe; `[metrics] enabled` off switch. See
  `docs/METRICS.md`.
- **LLM gateway upgrades** — OpenAI function-calling across every provider,
  token-level streaming passthrough for OpenAI-compatible upstreams, and
  Anthropic-native streaming translated to OpenAI chunks on the fly.
- **MCP** — resources (a live instance describes itself to agents) and
  WebSocket functions exposed as tools via ephemeral sessions.
- **`--dev` TUI** — live log search (`/`) + severity filter (`l`), a `?` help
  overlay, an Enter-to-open invocation inspector (recent calls per function),
  and a saturation column in the Processes tab.
- **Runtimes & scaffolding** — per-function environment variables
  (`[function.<name>.env]`); a `wasm-http` `init` template (a `wasm32-wasip1`
  handler in the WASI sandbox); the full-stack `ai-chat` template (React chat
  UI + a server-side agent loop through the gateway).
- **Per-worker seccomp-BPF** — a deny-EPERM blocklist of 22 escape/tamper
  syscalls in `pre_exec`, stacked on rlimits + `prctl` + Landlock.
- **Actionable startup errors** — bad `riz.toml` (points at the field), missing
  runtime binary (names it + install hint), and port-in-use (names the port +
  how to change it) now say how to fix themselves.
- **CI throughput floor** — a conservative, non-flaky regression tripwire for
  HTTP dispatch (the 91k req/s headline stays a `wrk` bench recipe).

### Changed

- **Safety-critical posture** — NASA's Power of 10, adapted to Rust
  (`docs/SAFETY.md`), is now binding for everything compiled into the binary:
  no `unwrap`/`expect`/`panic`/indexing/unchecked-arithmetic on runtime data,
  bounded channels, supervised loops. 253 flagged sites were driven to zero and
  the lints promoted to a three-tier enforced gate (workspace deny + a
  `--lib --bins` CI gate + a ratchet that only decreases).
- **Supply chain** — a `cargo-deny` CI gate, a CycloneDX SBOM, and keyless
  GitHub build-provenance attestations (SLSA via Actions OIDC) on every release
  artifact.
- **Static serving** streams file bodies (flat per-connection memory, no HEAD
  reads).
- **Production hardening (Phase 1)** — JWKS authorizer cache, reflected-origin
  credentialed-CORS rejection, and a hot-swap pool rebuild that resizes
  admission.

### Fixed

- WebSocket function routes now pass the per-caller API-key gate (they
  previously bypassed it via explicit route mounting); keyed requests bypass
  the response cache (no cross-caller serving); a startup warning fires when
  `[api_keys]` is set but `[auth] bearer_token` is unset (the `/_riz/*` plane,
  including MCP tool-calls, stays open otherwise).
- MCP no longer advertises WebSocket functions as directly callable tools.

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
