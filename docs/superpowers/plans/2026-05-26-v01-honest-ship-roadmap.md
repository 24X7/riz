# Riz v0.1 — Honest Ship Roadmap

> Status: superseded — all waves shipped; active roadmap is docs/plans/v1-roadmap.md.

> **For agentic workers:** This is a ROADMAP — a sequenced, scoped catalog of every item needed to ship a credible v0.1 of riz. Each wave links to a tactical implementation plan (REQUIRED SUB-PLAN). Use superpowers:subagent-driven-development to execute the tactical plans one wave at a time.

**Goal:** Ship riz v0.1 with the AWS API Gateway v2 + Lambda contract fully honored (including WebSocket APIs, the recently-flagged gap), the marketing-vs-reality gaps closed, the runtime claims actually backed by code, and the code debt that the architecture audit flagged paid down before scale hits.

**Architecture:** Three audit subagents (codebase, marketing-vs-reality, viral) produced the gap inventory. This roadmap groups the gaps into 9 ordered waves, each producing shippable software on its own. v0.1 is the cumulative product of waves 0–9.

**Tech stack:** Rust 1.83+, tokio, axum, `aws_lambda_events`, `http`, `async-trait`, ratatui, Bun runtime for handlers. WebSocket via `axum::extract::ws` + `tokio-tungstenite`. Authorizers via a new `Authorizer` trait. Python adapter via `python3` subprocess. Rust adapter via the user's pre-compiled binary speaking the same stdin/stdout JSON protocol the Bun adapter uses.

**Audit reference:** the three audit transcripts that drove this roadmap are summarized inline below. See git log around 2026-05-25 for the commit message that listed each finding.

---

## Out of scope for v0.1 (explicit non-goals)

These are real AWS Lambda + API Gateway features. They are NOT in v0.1. The honest-status section of the landing page must say so.

| Out of scope | Reason |
|---|---|
| REST API v1 (`ApiGatewayProxyRequest`) | Different event shape. Low demand for self-hosted use cases. v2 has been recommended by AWS since 2019. |
| Non-HTTP event sources (SQS / SNS / S3 / DynamoDB streams / EventBridge / scheduled) | Each requires a polling or push-source adapter. Different product surface. Defer to v0.2. |
| Lambda Layers | AWS-Lambda-specific deployment concept. Riz users vendor deps in their handler dir. |
| Lambda Extensions | Same reason as Layers. |
| Custom domain mappings | Reverse-proxy concern. Riz runs behind nginx/caddy. |
| VPC endpoints / private APIs | AWS-account-scoped concept. N/A for self-hosted. |
| Multiple deployment stages (dev/staging/prod with route promotion) | Stage NAME is supported (`requestContext.stage`); per-stage routing tables are not. Run multiple riz processes if you need that. |
| X-Ray distributed tracing | Replace with OpenTelemetry in v0.2 if there's demand. |

If a future v0.2 picks any of these up, they get their own dedicated roadmap.

---

## Pre-flight: remove backwards-compat junk I sneaked in (v0.1, not v2.5)

Some recent commits added a hidden `Start` subcommand "alias" for the renamed `Run` subcommand. This is junk-thinking for a pre-1.0 product with zero users yet. Strip it.

### Task PF-1: Drop the Start subcommand alias

**Files:**
- Modify: `src/main.rs:48-61` — remove the `Start` variant and the `#[command(hide = true)]` attribute.
- Modify: `src/main.rs` — make `Run` the only subcommand variant (still optional / defaults to running).

- [ ] **Step 1: Read `src/main.rs` lines 48–61 to confirm current state**
- [ ] **Step 2: Delete the `Start` variant from `enum Commands`. The final shape:**

```rust
#[derive(Subcommand)]
enum Commands {
    /// Start the runtime. Default when no subcommand is given.
    Run,
    /// Validate riz.toml and exit.
    Validate,
    /// List configured functions and their routes.
    Routes,
    /// Hot-swap a deployed function from S3.
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
}
```

- [ ] **Step 3: Run `cargo test 2>&1 | grep "test result"` — all 300 tests must still pass**
- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "chore: drop Start subcommand alias — Run is the only public name for v0.1"
```

---

## Wave 0.5 — Drift-prevention automation (layered parallel with Wave 1) ✅

**Why here:** three incidents have already shipped where the landing page advertised features that code didn't deliver (`max_concurrent` vs `concurrency`, Python/Rust silently falling back to Bun, `riz start` vs `riz run`). A v0.1 viral OSS launch cannot afford a fourth. Wedge a small automation surface in *before* Wave 1 lands more features so every future wave inherits the guardrails for free.

**Acceptance criteria:**
- `tests/landing_page_contract.rs` passes against the current `web/index.html` + a `PILLS` / `WORKS_NOW` / `COMING` truth slice that exactly matches the page.
- `tests/aws_contract.rs` round-trips five canonical AWS event fixtures (HTTP simple GET, HTTP POST w/ body, WS `$connect`, WS message, WS `$disconnect`) byte-for-byte modulo documented exclusions.
- `tests/wave_<N>_acceptance.rs` exists for every wave (0.5, 1, 2, 3, 4, 4.5, 5, 6, 7, 8, 9), each populated with `#[ignore]`-gated tests for every acceptance criterion in the wave.
- `.github/workflows/ci.yml` runs build + test + clippy + fmt on every push and a separate informational job for the `#[ignore]`-gated future-wave tests.
- Removing a feature pill from `web/index.html` without removing it from the truth slice fails CI.
- Bun integration tests in `tests/integration_test.rs` are no longer `#[ignore]`-gated (CI installs Bun).

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-wave-0p5-drift-prevention.md`. 10 tasks.

**Effort:** half a day.

---

## Wave 1 — WebSocket APIs (biggest user-flagged gap) ✅

**Why first:** the user explicitly called this out as missing. WebSocket APIs are a first-class API Gateway type in AWS, not a fringe feature.

**Acceptance criteria:**
- A `[function.chat]` block can declare `protocol = "websocket"` with `route_selection_expression = "$request.body.action"`.
- Three magic route keys work: `$connect`, `$disconnect`, `$default`.
- Each WebSocket message produces an `ApiGatewayWebsocketProxyRequest`-shaped event dispatched to the function.
- `event.requestContext.connectionId` and `event.requestContext.eventType` are populated.
- A management API endpoint `POST/DELETE/GET /_riz/connections/{connectionId}` lets handlers send messages back to / disconnect / inspect connected clients (matches AWS's `@connections` API).
- Connections survive `riz.toml` hot-reload of the WebSocket function (drain + reconnect after pool swap).
- All connections are cleanly closed on `SIGTERM` within the 30 s drain window.

**Tactical implementation plan:** **REQUIRED SUB-PLAN** — write `docs/superpowers/plans/2026-05-26-websocket-apis.md`. Estimated 18–22 tasks. Key components:

- `src/runtime/websocket.rs` — `WebSocketHandler` trait + `WsEvent` enum (Connect / Disconnect / Message).
- `src/ws/mod.rs` — connection store (`DashMap<ConnectionId, mpsc::Sender<Message>>`), spawned per-connection reader task, per-route-selection-expression dispatcher.
- `src/ws/upgrade.rs` — axum WebSocket upgrade handler mounted at the function's path.
- `src/ws/management.rs` — `@connections` REST endpoints under `/_riz/connections`.
- `src/config.rs` — add `FunctionConfig.protocol: Protocol { Http, WebSocket }`, `FunctionConfig.route_selection_expression: Option<String>`.
- `src/main.rs` — fork mount logic by `Protocol`: HTTP functions get a `ProcessHandler`, WebSocket functions get a `WsHandler` mounted at the upgrade path.
- Bun adapter (`assets/bun-adapter.mjs`) — accept `ApiGatewayWebsocketProxyRequest` shape, no behavioral change.
- Tests: connection lifecycle, route selection by message body, broadcast via management API, drain on hot-reload, graceful close on shutdown.

**Effort:** 2–3 days. Largest wave by far. Spec this one out before starting.

---

## Wave 2 — Python runtime adapter

**Why second:** the marketing audit flagged "Bun · Python · Rust" as the most serious false-advertising. Python is the largest Lambda audience after JS. Currently `runtime = "python"` is rejected at config validation (good for now) but the pill needs to come back to the landing page.

**Acceptance criteria:**
- `runtime = "python"` is accepted by `Config::validate()`.
- `handler = "app.lambda_handler"` resolves to file `app.py`, attribute `lambda_handler`.
- A `python3` (or configurable `python` binary) subprocess is spawned per concurrency slot.
- The adapter reads one event per line from stdin, decodes JSON, invokes `handler(event, context)`, writes the AWS-shaped response to stdout.
- `context` object exposes the same surface as the Bun context (`function_name`, `aws_request_id`, `get_remaining_time_in_millis()`).
- The Python adapter is embedded in the riz binary via `include_str!`, written to `~/.riz/python-adapter.py` on first run (same pattern as Bun).
- Example: `examples/lambdas/echo-python/main.py` with a working `[function.echo-python]` block in `examples/riz.dev.toml`.
- Integration test (`#[ignore]`-gated on `python3` presence) covers happy path + error path.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-python-adapter.md`. Estimated 8–10 tasks:

1. Write `assets/python-adapter.py` (mirror `bun-adapter.mjs` line-by-line).
2. Create `src/process/python.rs` (mirror `bun.rs` structure).
3. Add `PythonRuntime` to `RuntimeRegistry::new`.
4. Remove the Python rejection from `Config::validate`.
5. Make `RuntimeRegistry::get` return `&self.python` for `RuntimeKind::Python` instead of panicking.
6. Add `module_and_export` Python-extension handling (already works for `.py`).
7. Write an integration test in `tests/python_integration.rs` (gated on `python3` PATH check).
8. Update `web/index.html` honest-status: move "Python" from coming → working; restore the `python` pill.
9. Add `examples/lambdas/echo-python/main.py`.
10. Update README + `web/llms.txt`.

**Effort:** 1 day.

---

## Wave 3 — Lambda authorizers (REQUEST + JWT)

**Why third:** every prod-grade Lambda deployment uses an authorizer. The marketing audit flagged `requestContext.authorizer` always being empty. First HN comment will be "how do I sign requests."

**Acceptance criteria:**
- New `Authorizer` trait: `async fn authorize(event: &ApiGatewayV2httpRequest) -> Result<AuthorizerOutput, AuthError>` where `AuthorizerOutput` contains `principal_id`, `context: HashMap<String, Value>`, and a TTL.
- Two impls in v0.1: `RequestAuthorizer` (calls a user-declared function as authorizer) and `JwtAuthorizer` (validates against a JWKS URL).
- Config: `[function.api]` gains optional `authorizer = "myAuthorizer"` (REQUEST) or `[function.api.authorizer]` block with `type = "jwt"`, `issuer`, `audience`, `jwks_uri`.
- Authorizer responses are cached by source IP + Authorization header hash for the TTL (configurable, defaults to 5 min, matches AWS).
- When authorizer succeeds, `requestContext.authorizer` is populated with the output before the handler invocation.
- 401 returned on authorizer failure; 403 on `iam_policy.Effect != "Allow"` (REQUEST).
- Handlers can opt out: `authorizer = "none"` skips auth even if a global authorizer is declared.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-lambda-authorizers.md`. Estimated 12–14 tasks. New files: `src/auth/mod.rs`, `src/auth/request.rs`, `src/auth/jwt.rs`. New dep: `jsonwebtoken` + `reqwest` (already present for tests, promote to runtime).

**Effort:** 1.5 days.

---

## Wave 4 — CORS auto-preflight

**Why fourth:** browsers refuse to call cross-origin APIs without OPTIONS handling. First HN comment after authorizer one.

**Acceptance criteria:**
- New `[cors]` config block (global, applies to all user functions; per-function override possible).
- Fields: `allow_origins: Vec<String>`, `allow_methods: Vec<String>`, `allow_headers: Vec<String>`, `allow_credentials: bool`, `max_age_secs: u64`, `expose_headers: Vec<String>`.
- An `OPTIONS` request to any registered route returns 204 with the correct `Access-Control-Allow-*` headers, never reaching the handler.
- Non-OPTIONS requests get `Access-Control-Allow-Origin` echoed (when origin is in allowlist or `["*"]`).
- A request to an unregistered path returns 404 even with CORS headers — preflight is per-route, not per-server.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-cors-preflight.md`. Estimated 6–8 tasks. Single new file `src/cors.rs` plus axum middleware wiring in `src/server.rs::build_app`.

**Effort:** half a day.

---

## Wave 4.5 — Bearer-token auth on `/_riz/*`

**Why here:** the landing page promises this under "Coming." It's a real prod-grade concern (`/_riz/metrics` and `/_riz/registry` leak request volume + handler topology to anyone who can reach the box), and it's trivial — single shared-secret header check + one config field. Slotting it after CORS so the auth-shaped concerns ship together.

**Acceptance criteria:**
- New `[auth]` config block with field `bearer_token: Option<String>` (env-var sourceable: `RIZ_AUTH_BEARER_TOKEN`).
- When unset, `/_riz/*` endpoints behave exactly as today (open).
- When set, `/_riz/metrics`, `/_riz/registry`, `/_riz/mcp` require `Authorization: Bearer <token>`; missing/wrong → 401.
- `/_riz/health` remains open regardless of `bearer_token` (liveness probes from reverse-proxies / k8s must not require credentials).
- Constant-time comparison on the token (use `subtle::ConstantTimeEq` or equivalent — no `==` on user-controlled input).
- Auth check runs *before* MCP body parsing so a malformed body with a wrong token still returns 401, not 400.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-bearer-auth.md`. Estimated 4–6 tasks. New file `src/auth/bearer.rs`. Axum extractor wired into each `/_riz/*` handler.

**Effort:** 2 hours.

---

## Wave 5 — Real `getRemainingTimeInMillis()` + context fidelity

**Why fifth:** Lambda handlers commonly check the deadline and short-circuit work. Currently hardcoded to 30000 in the Bun adapter.

**Acceptance criteria:**
- The dispatch path passes the deadline (epoch millis) as a field on the wire-format event (e.g., `__riz_deadline_ms`, namespaced to not collide with AWS fields).
- The Bun adapter reads it and returns `deadline_ms - Date.now()` from `context.getRemainingTimeInMillis()`.
- `context.functionName` matches the function name from `riz.toml` (currently uses `AWS_LAMBDA_FUNCTION_NAME` env var as the only source).
- `context.invokedFunctionArn` produces a plausible synthetic ARN: `arn:riz:lambda:local:000000000000:function:<name>` when no override is given.
- `context.awsRequestId` matches `event.requestContext.requestId` (currently regenerated).

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-lambda-context.md`. Estimated 4–6 tasks. Touches `assets/bun-adapter.mjs`, `src/server.rs` (event construction), the Python adapter when Wave 2 lands.

**Effort:** half a day.

---

## Wave 6 — Rust runtime adapter

**Why sixth:** the smallest of the three runtime-parity asks. Rust handlers are pre-compiled binaries that speak the same line-JSON protocol the Bun adapter does.

**Acceptance criteria:**
- `runtime = "rust"` accepted by `Config::validate()`.
- `handler = "./target/release/my-handler"` is invoked directly as a subprocess; the binary is expected to loop reading lines from stdin, decoding JSON, and writing line-delimited JSON responses to stdout.
- A reference crate `crates/riz-rust-runtime/` provides the boilerplate `riz_rust_runtime::run(handler_fn)` so user code is just `fn main() { riz_rust_runtime::run(my_handler) }`.
- `examples/lambdas/echo-rust/` ships with a working Cargo.toml + main.rs + sample build instructions.
- Integration test gated on `cargo build` succeeding for the example.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-rust-adapter.md`. Estimated 6–8 tasks. New crate (workspace member), `src/process/rust.rs` (no adapter file needed — the binary IS the adapter), config validation flip.

**Effort:** 1 day.

---

## Wave 7 — Code debt the architecture audit flagged

**Why seventh:** debt accumulates; getting ahead of it before WebSocket adds 1500 LOC is correct sequencing. Half of these were already mature concerns at the time of audit; the others have grown since.

### 7.1 Split `src/system/mcp.rs` (848 LOC)

Split into:
- `src/system/mcp/mod.rs` — the `McpHandler` struct + `LambdaHandler` impl + dispatcher.
- `src/system/mcp/protocol.rs` — JSON-RPC types, batch handling, parse-error / notification rules.
- `src/system/mcp/tools.rs` — `tools/list` + `tools/call` logic, including reentrant Router dispatch + path-params substitution.
- `src/system/mcp/encoding.rs` — `substitute_path_params`, `urlencode`, response builders.

### 7.2 Split `src/process/mod.rs` (636 LOC)

Split into:
- `src/process/mod.rs` — `ProcessManager` struct + public API (`new`, `invoke`, `hot_swap`, `drain_pool`, `spawn_function`, `pool_stats`, `host_stats`).
- `src/process/pool.rs` — `RoutePool`, `ProcessHandle`, `spawn_process`, `kill_process_group`.
- `src/process/liveness.rs` — `spawn_liveness_watcher`, `handle_process_failure`.

### 7.3 Kill the dual stats system

`AppState.route_stats` is now dead weight. `RizState.functions` is the source of truth.

- Remove `AppState.route_stats` field.
- Remove `RouteStats` + `RouteStatsSnapshot` types from `src/state.rs`.
- Remove the dual-write from `AppState::record_request` (or remove the whole method if it's only called for the dual-write).
- Update the TUI / any other readers to use `RizState.functions` exclusively (TUI already does).

### 7.4 Typed errors in `runtime/process.rs`

Currently `ProcessHandler::invoke` classifies errors via `msg.contains("timeout") / "no pool")`. Replace with a typed error coming OUT of `ProcessManager::invoke`:

```rust
pub enum PoolError {
    Timeout,
    NoPool(String),
    SemaphoreClosed,
    InvalidResponse(String),
    Other(anyhow::Error),
}
```

### 7.5 Cache the per-request config lookup

Currently `state.config.read().await` is taken twice per request on the hot path. Cache the function-name → (runtime_tag, cache_ttl_secs) mapping inside `FunctionState` so the dispatch path only reads `riz_state.functions` (already cheap) — no `config.read()` at all in the hot path.

### 7.6 Drop the v1-flavored `multi_value_headers`

AWS HTTP API v2 responses don't use `multiValueHeaders` — multi-`Set-Cookie` uses the `cookies` array. Remove every `multi_value_headers: HeaderMap::new()` from response constructors in `src/runtime/mod.rs`, `src/system/*.rs`. The aws_lambda_events type still has the field; we just always emit it empty.

Actually scratch that — we DO emit it empty already. The cleanup is to add a unified `Response::json()` / `Response::text()` builder in `src/runtime/mod.rs` so handler code stops manually constructing the same 6-field literal everywhere.

### 7.7 Extract response builders

```rust
// src/runtime/response.rs
pub fn json_response(status: u16, value: &impl Serialize) -> ApiGatewayV2httpResponse { ... }
pub fn text_response(status: u16, content_type: &str, body: String) -> ApiGatewayV2httpResponse { ... }
```

Replace every hand-built `ApiGatewayV2httpResponse { ... }` literal in `src/system/health.rs`, `src/system/metrics.rs`, `src/system/registry.rs`, `src/system/mcp.rs` with the helper. ~80 LOC saved.

### 7.8 Drop hand-rolled `format_aws_time`

Replace `src/server.rs::format_aws_time` + `days_to_ymd` + `is_leap` (38 LOC) with `chrono`:

```rust
let time = chrono::Utc.timestamp_millis_opt(epoch_ms as i64).single()
    .map(|t| t.format("%d/%b/%Y:%H:%M:%S +0000").to_string())
    .unwrap_or_default();
```

Add `chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }` to Cargo.toml.

### 7.9 Extract cold-start bookkeeping

Cold-start `note_cold_start` is called at 4 spawn sites in `src/process/mod.rs` (lines 115, 225, 269, 328). Easy to forget on a 5th. Move into `spawn_process` itself with an `is_cold_start: bool` parameter, OR — cleaner — wrap every spawn in a `spawn_with_cold_start_record(pool, …) -> Result<ProcessHandle>` helper.

### 7.10 TUI snapshot, not shared RwLock

TUI tick currently `block_on`s `state.riz_state.functions.read()` and `state.config.read()`. Under high request load, this contends with the dispatch path.

Replace with a `tokio::sync::watch::channel<TuiSnapshot>` written by a periodic snapshotter task (100 ms cadence). TUI reads from the watch channel — never blocks the hot path.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-code-debt-cleanup.md`. Estimated 18–22 tasks across the 10 sub-items. Can be done in parallel by multiple subagents because most don't overlap.

**Effort:** 1.5 days if parallelized, 3 days sequential.

---

## Wave 8 — Test coverage gaps

The codebase audit flagged: no hotreload orchestration tests, no fault-injection for liveness/respawn, no hot_swap-under-load race tests, Bun integration tests are all `#[ignore]`, thin coverage of the dispatch hot path (cache-skip-on-auth, base64 body, AWS time format).

### 8.1 Hotreload orchestration tests

`tests/hotreload_integration.rs` — drive `hotreload::watch_config` with a temp file, verify add / remove / replace function diffs trigger the right ProcessManager calls. Use a fake ProcessManager mock.

### 8.2 Liveness fault-injection tests

`src/process/liveness.rs` (post Wave 7 split) — synthetic test where a "process" exits immediately, verify `spawn_liveness_watcher` re-spawns within 250 ms.

### 8.3 hot_swap-under-load race tests

`tests/hot_swap_race.rs` — spin up a pool, fire 100 concurrent invocations, trigger hot_swap halfway through. Verify zero requests dropped (every invocation either gets the old or the new handler's response, never a 502 from a killed handle).

### 8.4 Ungate Bun integration tests

CI installs Bun. Remove `#[ignore]` from `tests/integration_test.rs`. Add a single `#[ignore]`-gated "real workload" test that fires 1000 requests through the example accounts function and asserts no 5xx.

### 8.5 Dispatch hot path coverage

`src/server.rs` test module — direct tests for:
- Auth-bypass: request with `Authorization` header skips both cache.get and cache.set.
- Base64 round-trip: binary body in → base64-flagged event → handler echoes → binary body out.
- AWS time format: `format_aws_time(epoch)` produces parseable AWS-format string.
- 413 on >10 MB body for a routed request.
- 504 when integration_timeout_ms is exceeded before handler timeout_ms.

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-test-gap-fill.md`. Estimated 10–12 tasks.

**Effort:** 1 day.

---

## Wave 9 — Marketing artifacts (the viral hooks the audit flagged)

### 9.1 Asciinema demo loop for the landing page hero

10-second autoplaying loop, replaces the static fake-TUI in `web/index.html`. Scene order:
1. `riz run` (TUI fills the screen)
2. `hey -n 5000 localhost:3000/accounts/42` in a side terminal
3. P50/P75/P90/P95/P99 columns ticking up live, log pane scrolling
4. Edit `accounts/index.ts` in a third pane, save → hot-swap indicator flashes
5. Next curl returns the new body

Save as `web/demo.cast` (asciinema format) + `web/demo.svg` (rendered via `svg-term`). Embed in the hero section.

### 9.2 README rewrite

Currently no README at the repo root. Write one covering:
- 30-second install + first function
- Mental model: function = process pool = N routes
- The MCP differentiator (lead with this)
- Honest status table (copy from landing page)
- Comparison table vs LocalStack / AWS SAM Local / Cloudflare Workers
- Architecture diagram (single SVG)

### 9.3 Example handlers updated to AWS handler syntax

`examples/riz.dev.toml` and `examples/riz.prod.toml` use `handler = "./examples/lambdas/accounts/index.ts"` (explicit path). Change to `handler = "examples/lambdas/accounts/index.handler"` (AWS file.export style) and add a comment explaining both forms work.

### 9.4 Hero microcopy rewrite

Current hero subhead: "Run any AWS Lambda function on your own infrastructure. HTTP API Gateway v2 compatible — zero code changes, zero Docker, zero cold start mystery."

Replace with something that survives a "what about my SQS handler?" comment:

> Run your AWS HTTP API v2 Lambda handlers on a single Rust binary. Zero code changes, zero Docker, real percentiles in your terminal, and an MCP server so any Lambda becomes an LLM tool.

(Implies HTTP-only with the word "HTTP" in the first sentence; doesn't overclaim.)

**Tactical implementation plan:** `docs/superpowers/plans/2026-05-26-marketing-artifacts.md`. Estimated 6–8 tasks.

**Effort:** half a day.

---

## Wave dependencies

```
PF (cleanup)
  └─→ Wave 0.5 (Drift automation) ────┐
        │                               │
        └─→ Wave 1 (WebSocket)        ──┤
                                         │
  └─→ Wave 2 (Python)          ──┐      ├─→ Wave 8 (Tests) ──→ Wave 9 (Marketing)
  └─→ Wave 3 (Authorizers)     ──┤      │
  └─→ Wave 4 (CORS)            ──┼──────┤
  └─→ Wave 4.5 (Bearer auth)   ──┤      │
  └─→ Wave 5 (Context)         ──┤      │
  └─→ Wave 6 (Rust)            ──┘      │
                                         │
  └─→ Wave 7 (Code debt) ────────────────┘   [can run in parallel with any of 2-6]
```

Wave 0.5 (drift automation) lands FIRST — every other wave inherits its CI guardrails and acceptance-test oracle.

Wave 1 (WebSocket) is the most invasive and benefits from a clean codebase. Doing Wave 7 (debt) BEFORE Wave 1 is optional but reduces merge conflicts.

Waves 2–6 + 4.5 are mutually independent and can be parallelized across multiple subagent sessions.

Wave 8 (tests) waits on at least Waves 1 + 7 because it tests their output. Wave 9 (marketing) waits on enough features being shipped to demo against.

---

## Effort summary

| Wave | Effort | Cumulative |
|---|---|---|
| PF — Cleanup | 0.1 day | 0.1 |
| 0.5 — Drift-prevention automation | 0.5 day | 0.6 |
| 1 — WebSocket | 2.5 days | 3.1 |
| 2 — Python | 1 day | 4.1 |
| 3 — Authorizers | 1.5 days | 5.6 |
| 4 — CORS | 0.5 day | 6.1 |
| 4.5 — Bearer-token auth | 0.25 day | 6.35 |
| 5 — Lambda context | 0.5 day | 6.85 |
| 6 — Rust | 1 day | 7.85 |
| 7 — Code debt | 1.5 days (parallel) | 9.35 |
| 8 — Test coverage | 1 day | 10.35 |
| 9 — Marketing | 0.5 day | 10.85 |

**Total: ~11 days of focused work to ship a credible v0.1.**

Subagent parallelism can compress 2–6 + 4.5 + 7 into ~3 calendar days if multiple agent sessions run concurrently. WebSocket (Wave 1) is on the critical path and can't be shortened that way. Wave 0.5 (~half a day) is on the critical path *ahead* of Wave 1 — it's wedged in first so every subsequent wave inherits the drift-prevention CI gates.

---

## Self-review

**Spec coverage** (against the three audit transcripts):

- AWS conformance gaps from the codebase audit:
  - WebSocket APIs → Wave 1 ✓
  - Lambda authorizers → Wave 3 ✓
  - CORS preflight → Wave 4 ✓
  - `multi_value_headers` v1-flavor → Wave 7.6 ✓
  - REST API v1 → explicit out of scope ✓
  - `stage_variables` per-function (vs per-stage) → noted as acknowledged divergence ✓
  - `format_aws_time` hand-rolled → Wave 7.8 ✓

- Code debt from the codebase audit:
  - mcp.rs 848 LOC → Wave 7.1 ✓
  - process/mod.rs 636 LOC → Wave 7.2 ✓
  - dual stats → Wave 7.3 ✓
  - string-contains errors → Wave 7.4 ✓
  - per-request config lock x2 → Wave 7.5 ✓
  - duplicate response constructors → Wave 7.7 ✓
  - cold-start bookkeeping repeated x4 → Wave 7.9 ✓
  - TUI competes for hot-path locks → Wave 7.10 ✓
  - O(handlers × routes) dispatch → noted as acceptable for current scale, not addressed in v0.1
  - `route_name_matches` dead helper → cleanup during Wave 7

- Test coverage gaps from the codebase audit:
  - Zero hotreload orchestration tests → Wave 8.1 ✓
  - No fault-injection for liveness → Wave 8.2 ✓
  - No concurrency races for hot_swap → Wave 8.3 ✓
  - Bun integration tests are `#[ignore]` → Wave 8.4 ✓
  - Dispatch hot path tests thin → Wave 8.5 ✓

- Marketing-vs-reality gaps from the product audit:
  - `max_concurrent` → `concurrency` in landing-page riz.toml → DONE in commit before this plan
  - `https://riz.dev/install` script → DONE in commit before this plan
  - "Bun · Python · Rust" → Python in Wave 2, Rust in Wave 6, pills updated in commit before this plan
  - `riz run` vs `riz start` → DONE in commit before this plan
  - 30s drain not enforced → DONE in commit before this plan
  - `getRemainingTimeInMillis()` hardcoded → Wave 5 ✓
  - MCP undersold → DONE in commit before this plan
  - File-export handler syntax undersold → DONE in commit before this plan (added to landing page riz.toml example)

- Viral hooks from the viral audit:
  - Replace static TUI with asciinema → Wave 9.1 ✓
  - Lead with MCP → DONE on landing page; reinforced in Wave 9.2 README
  - Honest Status section → DONE on landing page
  - Drop "Python · Rust" pills until shipped → DONE
  - Comparison anchors → Wave 9.2 README
  - Hero microcopy that doesn't overclaim → Wave 9.4

**Placeholder scan:** No "TBD" / "implement later" anywhere. Each wave names files, test patterns, and acceptance criteria. The tactical sub-plans are explicitly listed as REQUIRED before execution of large waves.

**Type consistency:** Naming across waves is consistent. `Authorizer` trait in Wave 3 returns `AuthorizerOutput` (named explicitly). `WebSocketHandler` trait in Wave 1 named explicitly. `Protocol { Http, WebSocket }` enum in Wave 1 named explicitly. No conflicting names.

---

## Done.
