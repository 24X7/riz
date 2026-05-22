# Riz System Functions + LambdaHandler Foundation — Design Spec

> Spec A in a 3-spec sequence. Specs B (config schema migration) and C (type system polish) are explicitly out of scope; see the "Out of Scope" section.

## Goal

Introduce a `LambdaHandler` trait that unifies dispatch for user functions and built-in system functions. Add four system endpoints mounted under `/_riz/*` that expose runtime health, Prometheus metrics, the route registry, and an MCP server — all without requiring any change to user handler code.

Drop-in compatibility is non-negotiable: existing Lambda handlers must continue to run unchanged. No new exports, no new metadata, no opt-in protocol.

## Driving Constraints (from `web/index.html` positioning)

1. **"Your existing Lambda runs on Riz unchanged."** Zero changes to user handler code or runtime adapters.
2. **"P50–P99 latency, live logs, perf graphs."** Per-function percentiles are a marketed feature.
3. **"Single binary."** No new runtime dependencies (allowed: pure-Rust crates already in the dependency graph or one new pure-Rust crate at most).

## Architecture

A `LambdaHandler` trait becomes the universal contract. Every route — user function or system function — is served by a struct implementing the trait. The router holds `Vec<Arc<dyn LambdaHandler>>` and dispatches by iterating handlers in mount order; each handler is asked whether the request matches one of its declared routes; first match wins.

### Mount Order (fixed)

1. `HealthHandler`
2. `MetricsHandler`
3. `RegistryHandler`
4. `McpHandler`
5. One `ProcessHandler` per entry in `riz.toml`'s `[[routes]]` array, in definition order

System handlers mount first so `/_riz/*` always beats any user route. A user route whose path starts with `/_riz/` is rejected at config-load time.

### Component Responsibilities

| Component | File | What it does |
|-----------|------|--------------|
| `LambdaHandler` trait | `src/runtime/mod.rs` | Defines `routes()`, `invoke(event)`, `name()` |
| `ProcessHandler` | `src/runtime/process.rs` | Owns one route's `Arc<RoutePool>`, ports current `ProcessManager.invoke` flow |
| `RizState` / `FunctionState` | `src/state.rs` | Replaces `route_stats`, adds `LatencyWindow` |
| `LatencyWindow` | `src/state.rs` | 5-minute VecDeque of (Instant, latency_ms); computes p50/p75/p90/p95/p99 |
| `Router` (refactored) | `src/router.rs` | Holds `Vec<Arc<dyn LambdaHandler>>`, dispatches by trait |
| `HealthHandler` | `src/system/health.rs` | GET `/_riz/health` |
| `MetricsHandler` | `src/system/metrics.rs` | GET `/_riz/metrics` (Prometheus text format) |
| `RegistryHandler` | `src/system/registry.rs` | GET `/_riz/registry` (JSON manifest) |
| `McpHandler` | `src/system/mcp.rs` | POST `/_riz/mcp` (JSON-RPC; tools/list + tools/call) |

`ProcessManager` is removed as a top-level type. Its current responsibilities (pool spawning, hot-swap, liveness watchers, pool stats) become methods on `ProcessHandler` or move to an internal `Pool` module shared between `ProcessHandler` instances if needed.

## Type System

### LambdaHandler trait

```rust
#[async_trait::async_trait]
pub trait LambdaHandler: Send + Sync {
    /// Stable name for logs, metrics, and registry display.
    fn name(&self) -> &str;

    /// Routes this handler serves. Each (method, path) tuple is checked
    /// against the incoming request; ANY matches every method.
    fn routes(&self) -> &[RouteEntry];

    /// Invoke with a fully-built event. Returns a Response or a HandlerError
    /// (which the router converts to a 4xx/5xx response).
    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError>;
}

#[derive(Clone, Debug)]
pub struct RouteEntry {
    pub method: RouteMethod,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMethod {
    Any,
    Get, Post, Put, Delete, Patch, Head, Options,
}
```

**Type-name preservation:** This spec keeps `GatewayRequest`/`GatewayResponse` (not `ApiGatewayV2Event`/`Response`). Renames belong in Spec C. The trait does not gain v2 fields like `account_id` or `multi_value_headers` — those are Spec C territory.

### HandlerError

```rust
#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("timeout after {0}ms")]
    Timeout(u64),
    #[error("overloaded (max_concurrent={0})")]
    Overloaded(usize),
    #[error("process error: {0}")]
    Process(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl HandlerError {
    pub fn status_code(&self) -> u16 { /* 504 / 429 / 502 / 500 / 500 */ }
    pub fn to_response(&self) -> GatewayResponse { /* json error body */ }
}
```

## State Model

### RizState

```rust
pub struct RizState {
    pub functions: tokio::sync::RwLock<IndexMap<String, Arc<FunctionState>>>,
    pub start_time: std::time::Instant,
    pub version: &'static str,  // env!("CARGO_PKG_VERSION")
}
```

- **Map key:** `route_key` ("METHOD /path"). This preserves the current keying. Spec B will migrate to function names when multi-route-per-function lands.
- **`IndexMap`:** preserves `riz.toml` insertion order so the TUI and registry render in a stable, user-meaningful order.
- **`Arc<FunctionState>`:** read lock the outer map to look up, then bump atomics lock-free — same fast-path pattern landed in BUG-15.

### FunctionState

```rust
pub struct FunctionState {
    pub route_key: String,
    pub route: Option<RouteConfig>,  // Some for user functions, None for system
    pub kind: FunctionKind,
    pub invocations: AtomicU64,
    pub errors: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub cold_starts: AtomicU64,
    pub healthy: AtomicBool,
    pub last_invoked: std::sync::Mutex<Option<std::time::Instant>>,
    pub latency: std::sync::Mutex<LatencyWindow>,
}

pub enum FunctionKind { User, System }
```

`route` is `Option<RouteConfig>` because system handlers have no `RouteConfig` (no handler binary, no runtime, no concurrency limit). The registry serializes `null` for these fields when `kind == System`.

The hot path:

```rust
state.record_invocation(&route_key, latency_ms, healthy, cache_hit);
```

reads the outer `RwLock` for lookup, atomic-bumps invocations/errors/cache_*, takes the latency `Mutex` briefly to push one sample, takes `last_invoked` Mutex briefly to set the timestamp. No write lock contention on the hot path after first invocation.

### LatencyWindow

```rust
pub struct LatencyWindow {
    samples: std::collections::VecDeque<(std::time::Instant, f64)>,  // (timestamp, ms)
    capacity_hint: usize,  // soft cap; bounded to prevent OOM under attack
}

impl LatencyWindow {
    const WINDOW_SECS: u64 = 300;
    const MAX_SAMPLES: usize = 100_000;  // hard cap, ~2.4 MB worst case

    pub fn push(&mut self, now: std::time::Instant, latency_ms: f64);

    /// Returns (p50, p75, p90, p95, p99) for samples newer than 5 minutes.
    /// Empty window returns (0.0, 0.0, 0.0, 0.0, 0.0).
    pub fn percentiles(&mut self, now: std::time::Instant) -> (f64, f64, f64, f64, f64);

    /// Count of samples currently in the 5-min window.
    pub fn count(&mut self, now: std::time::Instant) -> usize;
}
```

`push` drops samples older than 5 minutes when at the hard cap. `percentiles` evicts stale samples first, then clones the live slice and sorts in-place to compute percentiles by linear interpolation (`nearest-rank` is acceptable for v0.1).

**Why VecDeque, not `hdrhistogram`:** stays in pure stdlib, no new dependency. At 100 req/s sustained, ~30K samples × 24 bytes = ~720 KB per function — fine.

## Request Lifecycle

```
1. axum receives request
2. server::dispatch_lambda builds GatewayRequest (existing logic preserved)
3. router.dispatch(&req, &state) is called
   - iterate state.handlers (Vec<Arc<dyn LambdaHandler>>)
   - for each handler, call .routes() and check (method, path) match
   - first match: call handler.invoke(req).await
   - no match: return GatewayResponse::error(404, "not found")
4. record_invocation writes to RizState
5. cache layer logic unchanged (set/get around the invoke as today)
6. response converted to axum::Response
```

The cache is wired around `router.dispatch`, not inside any specific handler — so cache behavior for user functions is identical to today. System handlers are also subject to the cache, but their per-route `cache_ttl_secs` defaults to 0 (no cache) in the implicit configs.

## System Function Specifications

### `GET /_riz/health`

**Behavior:** Always returns 200. Body is JSON describing runtime state:

```json
{
  "status": "ok",
  "version": "0.1.0",
  "uptime_secs": 12847,
  "functions": [
    {
      "route_key": "GET /api",
      "healthy": true,
      "invocations": 12847,
      "errors": 3,
      "p50_ms": 4.2,
      "p99_ms": 18.1,
      "last_invoked_secs_ago": 2.3
    }
  ]
}
```

**Why always 200:** distinguishes liveness (Riz process alive and responding) from per-function readiness (which is what the existing `/ready` endpoint covers). Load balancers can keep using `/health`.

### `GET /_riz/metrics`

**Behavior:** Returns Prometheus text format 0.0.4. Content-Type `text/plain; version=0.0.4`.

```
# HELP riz_invocations_total Total function invocations
# TYPE riz_invocations_total counter
riz_invocations_total{route="GET /api"} 12847

# HELP riz_errors_total Total function errors
# TYPE riz_errors_total counter
riz_errors_total{route="GET /api"} 3

# HELP riz_latency_ms Function latency percentiles (5-min window)
# TYPE riz_latency_ms summary
riz_latency_ms{route="GET /api",quantile="0.5"} 4.2
riz_latency_ms{route="GET /api",quantile="0.75"} 5.8
riz_latency_ms{route="GET /api",quantile="0.9"} 9.1
riz_latency_ms{route="GET /api",quantile="0.95"} 11.4
riz_latency_ms{route="GET /api",quantile="0.99"} 18.1

# HELP riz_cold_starts_total Total cold-start process spawns
# TYPE riz_cold_starts_total counter
riz_cold_starts_total{route="GET /api"} 7

# HELP riz_function_healthy Whether the function pool is healthy (1) or unhealthy (0)
# TYPE riz_function_healthy gauge
riz_function_healthy{route="GET /api"} 1

# HELP riz_uptime_seconds Runtime uptime
# TYPE riz_uptime_seconds gauge
riz_uptime_seconds 12847
```

**Datadog coexistence:** The existing `MetricsEmitter` (Datadog) is untouched. Both emit independently from `record_invocation`. Datadog can be silenced via existing `[datadog]` config; Prometheus can be silenced via a future `metrics_enabled` flag (Spec C scope, not Spec A).

### `GET /_riz/registry`

**Behavior:** Returns JSON manifest describing all mounted routes. Pure derivation from `RizState.functions` — no introspection of user code.

```json
{
  "version": "0.1.0",
  "functions": [
    {
      "route_key": "GET /api",
      "method": "GET",
      "path": "/api",
      "runtime": "bun",
      "kind": "user",
      "handler": "./src/api/index.ts",
      "timeout_ms": 30000,
      "concurrency": 10,
      "cache_ttl_secs": 0
    },
    {
      "route_key": "GET /_riz/health",
      "method": "GET",
      "path": "/_riz/health",
      "runtime": null,
      "kind": "system",
      "handler": null,
      "timeout_ms": null,
      "concurrency": null,
      "cache_ttl_secs": null
    }
  ]
}
```

**System functions exclude themselves from the registry only when serving the registry endpoint? No** — they are listed alongside user functions. This makes the manifest self-describing: an LLM consuming `/_riz/registry` learns that `/_riz/mcp` exists from `/_riz/registry` itself.

### `POST /_riz/mcp`

**Behavior:** JSON-RPC 2.0 over POST. Implements the MCP `tools/list` and `tools/call` methods only (v0.1 — MCP server lifecycle methods like `initialize` are deferred to a follow-up spec).

**`tools/list` response:** every user function (kind = User) becomes a tool. System functions are excluded from `tools/list` — they are operational endpoints, not user-callable tools.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [
      {
        "name": "GET_api",
        "description": "Invoke GET /api (bun runtime)",
        "inputSchema": {
          "type": "object",
          "properties": {
            "body":        { "type": "string",  "description": "Request body, raw or base64-encoded" },
            "headers":     { "type": "object",  "additionalProperties": { "type": "string" } },
            "queryParams": { "type": "object",  "additionalProperties": { "type": "string" } },
            "pathParams":  { "type": "object",  "additionalProperties": { "type": "string" } },
            "isBase64Encoded": { "type": "boolean", "default": false }
          }
        }
      }
    ]
  }
}
```

**Tool name format:** `METHOD_path` where `/` becomes `_` and `:param` is stripped (e.g., `GET /accounts/:id` → `GET_accounts_id`). Collisions cause startup error (caller of Spec B will validate this earlier; for Spec A, validate at McpHandler construction).

**`tools/call` flow:**
1. Look up the tool name in the registry → find the corresponding `route_key`.
2. Assemble a `GatewayRequest` from the tool call arguments (the generic envelope).
3. Re-enter `Router::dispatch` with the assembled request.
4. Return the `GatewayResponse` as the JSON-RPC result, wrapped in MCP's content shape:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      { "type": "text", "text": "{\"statusCode\":200,...}" }
    ],
    "isError": false
  }
}
```

`isError: true` if the underlying response had `status_code >= 400`. The full response body is stringified inside `content[0].text` — MCP clients can re-parse it.

**Reentry safety:** `McpHandler` holds `Arc<Router>` (the same router that dispatched the MCP request). Recursive dispatch is safe because the router has no per-request mutable state. A tool name resolving to the MCP route itself is rejected at validation time (no `MCP` tool in `tools/list`).

## Cold-Start Tracking

`cold_starts` increments inside `ProcessHandler` whenever `spawn_process` is called for a member of its pool. This happens at:

- Initial pool fill on startup
- Any restart triggered by `handle_process_failure`
- The new process during `hot_swap`

System handlers never increment cold starts.

## Configuration

`riz.toml` schema is **unchanged**. The four system endpoints are mounted automatically. There is no opt-out flag in Spec A; future `system_functions = false` belongs to Spec C alongside CLI restructure.

`RouteConfig` (existing type in `src/config.rs`) gains no new fields in Spec A.

## Auth

**None.** Documented in spec and in the eventual `web/llms.txt` update: operators must place Riz behind a reverse proxy if `/_riz/*` should not be public. A future Spec C may add an optional bearer token gate. This matches the user's original spec direction ("v0.2: optional bearer token; until then: reverse proxy").

This is a documented limitation, not a bug. Exposing `/_riz/mcp` publicly without a proxy lets anyone invoke user functions over MCP — which is the same blast radius as letting them call the HTTP routes directly anyway. Exposing `/_riz/registry` reveals the route shape. Both are operator responsibilities.

## Testing Strategy (drift prevention)

**Goal:** the refactor cannot silently change observable behavior. Tests anchor the contract at three layers.

### Layer 1 — HTTP boundary golden tests (added BEFORE refactor)

`tests/http_boundary.rs` — golden tests that fire HTTP requests against `build_app(state)` and assert response code, headers, and body shape. These tests are written FIRST (before any trait introduction), capturing current behavior of:

- `GET /health` → 200 with `{"status":"ok"}`
- `GET /ready` → 200 or 503 depending on pool health
- `POST /cache/invalidate` → returns evicted count
- `POST /deploy` without auth → 503
- A user function fallback route → dispatches through `process_manager.invoke` and returns whatever the lambda returns
- A request body exceeding 10 MB → 413
- A request with `Authorization` header → cache bypassed
- A binary request body → re-emitted as base64

After the refactor, every one of these tests must still pass with zero edits.

### Layer 2 — Trait-level unit tests

Each handler implementation gets unit tests that construct it directly (not through axum) and call `.invoke(&event)`:

- `ProcessHandler::invoke` round-trips a synthetic event through a stub `RoutePool`
- `HealthHandler::invoke` returns expected JSON shape against a fixture `RizState`
- `MetricsHandler::invoke` returns valid Prometheus text (parseable by `prometheus-parse` or equivalent regex)
- `RegistryHandler::invoke` returns expected JSON shape
- `McpHandler::invoke` for `tools/list` returns expected tool array
- `McpHandler::invoke` for `tools/call` correctly assembles event and dispatches (via a stub router)

### Layer 3 — Integration tests

`tests/system_functions_integration.rs` — full server with a stub user function, then:
- Hit `/_riz/health` and verify per-function stats appear after invoking the user function
- Hit `/_riz/metrics` and assert Prometheus output contains the user route's metrics
- Hit `/_riz/registry` and verify the user function is listed
- POST `/_riz/mcp` with a `tools/list` and verify the user function appears
- POST `/_riz/mcp` with a `tools/call` and verify the user function is invoked

### Layer 4 — Property-tested LatencyWindow

`LatencyWindow` gets property tests via `quickcheck` or hand-rolled randomized inputs:
- `percentiles` of N identical samples returns that sample for every quantile
- `percentiles` is monotone non-decreasing across quantiles
- Samples older than 300s are evicted on the next read
- Push count never exceeds `MAX_SAMPLES`

## Error Handling

Per-handler errors return `HandlerError` which the router maps to a `GatewayResponse` with the appropriate status code (504/429/502/500). Errors are logged at the router level with the route_key as a structured field. The `record_invocation` write happens regardless of success/error so error rates are tracked accurately.

System handlers should not panic. If MetricsHandler encounters a malformed FunctionState (shouldn't happen) it emits a metric line for what it can and skips the broken row, logging at WARN. If McpHandler receives malformed JSON-RPC, it returns a JSON-RPC error response, not a 500.

## File-Level Plan

**New files:**
- `src/runtime/mod.rs` — trait + RouteEntry + RouteMethod + HandlerError
- `src/runtime/process.rs` — ProcessHandler (moves logic from src/process/mod.rs)
- `src/system/mod.rs` — module root
- `src/system/health.rs`
- `src/system/metrics.rs`
- `src/system/registry.rs`
- `src/system/mcp.rs`

**Modified:**
- `src/state.rs` — RizState, FunctionState, LatencyWindow added; route_stats removed
- `src/router.rs` — refactored to hold `Vec<Arc<dyn LambdaHandler>>` and dispatch via trait
- `src/server.rs` — `dispatch_lambda` calls `router.dispatch()` instead of `process_manager.invoke()`
- `src/main.rs` — builds handler list, mounts system handlers first, then ProcessHandler per route
- `src/process/mod.rs` — `ProcessManager` may shrink to an internal `Pool`/`spawn_process` module re-exported from `runtime::process`. The existing pool/liveness/hot_swap logic is preserved verbatim, just relocated.
- `Cargo.toml` — adds `async-trait`, `thiserror`, `indexmap`

**Removed:**
- `AppState::route_stats` — replaced by `AppState::riz_state: Arc<RizState>`

## Out of Scope (explicitly deferred)

These are mentioned in the broader Runtime Spec but belong in follow-up specs:

- New `[function.<name>]` TOML schema with `[[function.<name>.routes]]` blocks → **Spec B**
- Hardcoded `params` injection per route → **Spec B**
- `memory_mb`, `system_functions`, `mcp_enabled`, `metrics_enabled` config flags → **Spec C**
- Rename `GatewayRequest`/`GatewayResponse` → `ApiGatewayV2Event`/`Response` → **Spec C**
- Full v2 fields (`account_id`, `api_id`, `stage`, `time` string, `user_agent`, `multi_value_headers`, `cookies`) → **Spec C**
- `riz dev` / `riz run` CLI subcommands with distinct defaults → **Spec C**
- Auth gating on `/_riz/*` (bearer token) → **Spec C**
- Function metadata introspection protocol (`__riz_control: "describe"`) → **future Spec D**
- Per-runtime adapters for Python/Rust handlers → already partial; full coverage future spec
- MCP server lifecycle methods (`initialize`, `shutdown`) → MCP v0.2

## Capabilities to Preserve

Same set as the broader Runtime Spec. After Spec A, all of these must remain true:

- HTTP API Gateway v2 format in/out, unchanged
- Single binary, no new runtime dependencies beyond pure-Rust crates
- Existing TOML schema works as today
- Process group kill (`killpg`) preserved
- Semaphore `try_acquire` preserved (never `acquire`)
- Graceful shutdown on SIGTERM with 30s drain preserved
- All 143 existing tests pass
- Function names beginning with `_riz` rejected at config-load time
