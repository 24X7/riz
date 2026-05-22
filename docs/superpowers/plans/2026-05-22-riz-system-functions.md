# Riz System Functions + LambdaHandler Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a `LambdaHandler` trait that unifies dispatch for user functions and four new built-in system endpoints (`/_riz/health`, `/_riz/metrics`, `/_riz/registry`, `/_riz/mcp`), with per-function P50/P75/P90/P95/P99 latency tracking, while preserving all existing behavior.

**Architecture:** Define a `LambdaHandler` trait. Port existing `ProcessManager.invoke` flow into a `ProcessHandler` struct that implements the trait. Refactor `Router` to hold `Vec<Arc<dyn LambdaHandler>>` and dispatch by trait. Add four new handler structs for the system endpoints. Replace `AppState.route_stats` with `Arc<RizState>` containing per-function counters, cold-start tracking, and a 5-minute `LatencyWindow` for percentiles.

**Tech Stack:** Rust, tokio, axum, async-trait, thiserror, indexmap, existing project conventions (Cargo.toml, src/, tests/).

**Drift-prevention strategy:** Task 1 captures current HTTP-boundary behavior as golden tests BEFORE any refactor. Every later task must keep those tests passing. Layer 2 (trait-level tests) and Layer 3 (integration tests) are added alongside their respective handlers.

---

## File Structure

**New files:**
- `src/runtime/mod.rs` — `LambdaHandler` trait, `RouteEntry`, `RouteMethod`, `HandlerError`
- `src/runtime/process.rs` — `ProcessHandler` (owns one route's pool)
- `src/system/mod.rs` — module root + shared MCP helpers (`mcp_tool_name`)
- `src/system/health.rs` — `HealthHandler`
- `src/system/metrics.rs` — `MetricsHandler` (Prometheus text)
- `src/system/registry.rs` — `RegistryHandler` (JSON manifest)
- `src/system/mcp.rs` — `McpHandler` (JSON-RPC)
- `tests/http_boundary.rs` — Layer 1 golden tests
- `tests/system_functions_integration.rs` — Layer 3 integration tests

**Modified:**
- `Cargo.toml` — add `async-trait`, `thiserror`, `indexmap`
- `src/state.rs` — add `RizState`, `FunctionState`, `LatencyWindow`; remove `route_stats`
- `src/router.rs` — refactor to hold `Vec<Arc<dyn LambdaHandler>>`
- `src/server.rs` — `dispatch_lambda` calls `router.dispatch()`
- `src/main.rs` — build handler list, mount system handlers first
- `src/process/mod.rs` — keep pool internals; remove `ProcessManager` public surface (re-export from `runtime::process`)
- `src/config.rs` — reject routes whose path starts with `/_riz/` at load time

**Unchanged:** `src/cache.rs`, `src/metrics.rs` (Datadog), `src/deploy.rs`, `src/hotreload.rs`, `src/tui/*`, `src/gateway.rs`, `src/process/bun.rs`, `src/process/runtime.rs`, `assets/bun-adapter.mjs`.

---

## Task 1: Pin current HTTP-boundary behavior

**Files:**
- Create: `tests/http_boundary.rs`

- [ ] **Step 1: Write the golden tests against current `build_app()`**

These tests bind to a random port and fire HTTP at the assembled app. They capture today's behavior so the refactor can't silently change it.

Create `tests/http_boundary.rs`:

```rust
//! Layer 1 — HTTP boundary golden tests. These pin the externally observable
//! behavior of the server BEFORE the LambdaHandler refactor. Every test must
//! still pass unchanged after the refactor.

use std::net::SocketAddr;
use std::sync::Arc;

fn make_state_with_routes(routes: Vec<riz::config::RouteConfig>) -> Arc<riz::state::AppState> {
    let mut config = riz::config::Config::default();
    config.routes = routes;
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let router = riz::router::Router::new(config.routes.clone());
    let process_manager = riz::process::ProcessManager::new();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
    })
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn health_returns_200_ok_json() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_200_when_all_pools_healthy() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/ready")).await.unwrap();
    // With no routes, no pools to be unhealthy
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/no-such-route")).await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn deploy_without_auth_returns_503() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/deploy"))
        .json(&serde_json::json!({
            "lambda": "x",
            "s3_bucket": "b",
            "s3_key": "k"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn cache_invalidate_with_keys_returns_evicted_count() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/cache/invalidate"))
        .json(&serde_json::json!({"keys":["nonexistent"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["evicted"].is_number());
}

#[tokio::test]
async fn oversized_body_returns_413() {
    let state = make_state_with_routes(vec![]);
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let big_body = vec![b'x'; 11 * 1024 * 1024]; // 11 MB > 10 MB limit
    let resp = client
        .post(format!("http://{addr}/anywhere"))
        .body(big_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
}
```

- [ ] **Step 2: Run tests to verify they pass against current code**

```bash
cargo test --test http_boundary 2>&1 | tail -10
```

Expected: 6 passed, 0 failed. These tests are the contract.

- [ ] **Step 3: Commit**

```bash
git add tests/http_boundary.rs
git commit -m "test: pin HTTP boundary behavior with golden tests"
```

---

## Task 2: Add dependencies + create empty module scaffolding

**Files:**
- Modify: `Cargo.toml`
- Create: `src/runtime/mod.rs`
- Create: `src/system/mod.rs`
- Modify: `src/main.rs:1-12` (add `mod runtime; mod system;`)

- [ ] **Step 1: Add dependencies to `Cargo.toml`**

Add these to `[dependencies]`:

```toml
async-trait = "0.1"
thiserror = "1"
indexmap = { version = "2", features = ["serde"] }
```

- [ ] **Step 2: Create `src/runtime/mod.rs` with placeholder content**

```rust
//! Riz runtime — LambdaHandler trait and the canonical request/response types.
//! All handlers (user functions and system functions) implement LambdaHandler.

pub mod process;
```

- [ ] **Step 3: Create `src/runtime/process.rs` with placeholder content**

```rust
//! ProcessHandler — owns one route's process pool and implements LambdaHandler.
//! Ports the existing ProcessManager.invoke flow into a trait implementation.
```

- [ ] **Step 4: Create `src/system/mod.rs` with placeholder content**

```rust
//! Riz system functions mounted under /_riz/*.
//! Each handler implements LambdaHandler and reads from RizState.

pub mod health;
pub mod metrics;
pub mod registry;
pub mod mcp;
```

- [ ] **Step 5: Create empty system handler files**

```bash
touch src/system/health.rs src/system/metrics.rs src/system/registry.rs src/system/mcp.rs
```

Put a single line in each so the module declarations compile:

`src/system/health.rs`:
```rust
//! /_riz/health handler.
```

`src/system/metrics.rs`:
```rust
//! /_riz/metrics handler — Prometheus text format.
```

`src/system/registry.rs`:
```rust
//! /_riz/registry handler — JSON manifest of mounted routes.
```

`src/system/mcp.rs`:
```rust
//! /_riz/mcp handler — JSON-RPC 2.0 (tools/list + tools/call).
```

- [ ] **Step 6: Wire modules into `src/main.rs`**

Find the existing `mod cache;` line at the top of `src/main.rs` and insert:

```rust
mod runtime;
mod system;
```

After the existing `mod` declarations, before `use` statements.

- [ ] **Step 7: Verify it builds**

```bash
cargo build 2>&1 | tail -5
```

Expected: builds cleanly, no errors.

- [ ] **Step 8: Run all existing tests**

```bash
cargo test 2>&1 | tail -8
```

Expected: 143 tests pass (no behavior change yet).

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/runtime src/system src/main.rs
git commit -m "scaffold: add runtime/ and system/ module trees + deps"
```

---

## Task 3: Implement LatencyWindow with property tests

**Files:**
- Modify: `src/state.rs` (append after existing code)

- [ ] **Step 1: Write failing tests for `LatencyWindow`**

Append to `src/state.rs` (after the existing `#[cfg(test)] mod tests` block, or inside it):

```rust
#[cfg(test)]
mod latency_window_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn empty_window_returns_zero_percentiles() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        let (p50, p75, p90, p95, p99) = w.percentiles(now);
        assert_eq!(p50, 0.0);
        assert_eq!(p75, 0.0);
        assert_eq!(p90, 0.0);
        assert_eq!(p95, 0.0);
        assert_eq!(p99, 0.0);
    }

    #[test]
    fn identical_samples_return_that_value() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for _ in 0..100 {
            w.push(now, 7.5);
        }
        let (p50, p75, p90, p95, p99) = w.percentiles(now);
        assert_eq!(p50, 7.5);
        assert_eq!(p75, 7.5);
        assert_eq!(p90, 7.5);
        assert_eq!(p95, 7.5);
        assert_eq!(p99, 7.5);
    }

    #[test]
    fn percentiles_are_monotonic() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for i in 1..=100 {
            w.push(now, i as f64);
        }
        let (p50, p75, p90, p95, p99) = w.percentiles(now);
        assert!(p50 <= p75, "p50={p50} p75={p75}");
        assert!(p75 <= p90, "p75={p75} p90={p90}");
        assert!(p90 <= p95, "p90={p90} p95={p95}");
        assert!(p95 <= p99, "p95={p95} p99={p99}");
    }

    #[test]
    fn linear_distribution_gives_expected_percentiles() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for i in 1..=100 {
            w.push(now, i as f64);
        }
        let (p50, _, _, p95, p99) = w.percentiles(now);
        // Nearest-rank: p50 of 1..=100 is the 50th element = 50
        assert!((p50 - 50.0).abs() < 1.0, "p50={p50}");
        // p95 ≈ 95
        assert!((p95 - 95.0).abs() < 1.0, "p95={p95}");
        // p99 ≈ 99
        assert!((p99 - 99.0).abs() < 1.0, "p99={p99}");
    }

    #[test]
    fn samples_older_than_window_are_evicted() {
        let mut w = LatencyWindow::new();
        let old = Instant::now();
        w.push(old, 1.0);
        let now = old + Duration::from_secs(301);
        w.push(now, 100.0);
        let (p50, _, _, _, _) = w.percentiles(now);
        // Only the new sample (100.0) is in window — 1.0 was evicted
        assert_eq!(p50, 100.0);
        assert_eq!(w.count(now), 1);
    }

    #[test]
    fn push_never_exceeds_max_samples() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for i in 0..(LatencyWindow::MAX_SAMPLES + 10_000) {
            w.push(now, i as f64);
        }
        assert!(w.raw_len() <= LatencyWindow::MAX_SAMPLES);
    }

    #[test]
    fn count_only_includes_live_samples() {
        let mut w = LatencyWindow::new();
        let old = Instant::now();
        for _ in 0..5 {
            w.push(old, 1.0);
        }
        let now = old + Duration::from_secs(301);
        for _ in 0..3 {
            w.push(now, 2.0);
        }
        assert_eq!(w.count(now), 3);
    }
}
```

- [ ] **Step 2: Run the failing tests**

```bash
cargo test latency_window 2>&1 | tail -10
```

Expected: compile errors — `LatencyWindow` doesn't exist yet.

- [ ] **Step 3: Implement `LatencyWindow`**

Add to `src/state.rs` (place after the existing imports near the top, before `pub struct AppState`):

```rust
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 5-minute rolling window of latency samples. Push on each invocation,
/// read percentiles on each metrics scrape or TUI render.
///
/// Memory: at 100 req/s sustained = ~30K samples × 24 bytes ≈ 720 KB per function.
/// Hard cap at MAX_SAMPLES prevents unbounded growth under attack.
pub struct LatencyWindow {
    samples: VecDeque<(Instant, f64)>,
}

impl LatencyWindow {
    pub const WINDOW: Duration = Duration::from_secs(300);
    pub const MAX_SAMPLES: usize = 100_000;

    pub fn new() -> Self {
        Self { samples: VecDeque::new() }
    }

    pub fn push(&mut self, now: Instant, latency_ms: f64) {
        if self.samples.len() >= Self::MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((now, latency_ms));
    }

    /// Evict samples older than WINDOW from the front.
    fn evict_stale(&mut self, now: Instant) {
        let cutoff = now.checked_sub(Self::WINDOW).unwrap_or(now);
        while let Some(&(t, _)) = self.samples.front() {
            if t < cutoff {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns (p50, p75, p90, p95, p99) over the live window.
    /// Empty window returns zeros. Uses nearest-rank with linear sort.
    pub fn percentiles(&mut self, now: Instant) -> (f64, f64, f64, f64, f64) {
        self.evict_stale(now);
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0, 0.0, 0.0);
        }
        let mut sorted: Vec<f64> = self.samples.iter().map(|&(_, v)| v).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |p: f64| -> f64 {
            // Nearest-rank: index = ceil(p * N) - 1, clamped.
            let n = sorted.len();
            let idx = ((p * n as f64).ceil() as usize).saturating_sub(1).min(n - 1);
            sorted[idx]
        };
        (q(0.50), q(0.75), q(0.90), q(0.95), q(0.99))
    }

    pub fn count(&mut self, now: Instant) -> usize {
        self.evict_stale(now);
        self.samples.len()
    }

    /// Raw sample count including stale entries — used by tests + soft-cap reasoning.
    pub fn raw_len(&self) -> usize {
        self.samples.len()
    }
}

impl Default for LatencyWindow {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test latency_window 2>&1 | tail -10
```

Expected: 7 passed, 0 failed.

- [ ] **Step 5: Run all tests to ensure no regression**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): add LatencyWindow with 5-min percentile tracking"
```

---

## Task 4: Add RizState + FunctionState; record_invocation helper

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Write failing tests for `RizState` and `FunctionState`**

Append to `src/state.rs`:

```rust
#[cfg(test)]
mod riz_state_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn function_state_starts_zeroed() {
        let f = FunctionState::user("GET /api", make_route_config());
        assert_eq!(f.invocations.load(Ordering::Relaxed), 0);
        assert_eq!(f.errors.load(Ordering::Relaxed), 0);
        assert_eq!(f.cache_hits.load(Ordering::Relaxed), 0);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 0);
        assert_eq!(f.cold_starts.load(Ordering::Relaxed), 0);
        assert!(f.healthy.load(Ordering::Relaxed));
        assert_eq!(f.kind, FunctionKind::User);
        assert!(f.route.is_some());
    }

    #[tokio::test]
    async fn system_function_state_has_no_route_config() {
        let f = FunctionState::system("GET /_riz/health");
        assert_eq!(f.kind, FunctionKind::System);
        assert!(f.route.is_none());
    }

    #[tokio::test]
    async fn register_then_record_increments_counters() {
        let state = RizState::new();
        state.register(FunctionState::user("GET /api", make_route_config())).await;
        state.record_invocation("GET /api", 12.3, true, false).await;
        state.record_invocation("GET /api", 7.5, true, false).await;
        let functions = state.functions.read().await;
        let f = functions.get("GET /api").unwrap();
        assert_eq!(f.invocations.load(Ordering::Relaxed), 2);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn record_invocation_for_unknown_route_is_noop() {
        let state = RizState::new();
        // Should not panic / not error
        state.record_invocation("GET /never-registered", 5.0, true, false).await;
    }

    #[tokio::test]
    async fn record_invocation_with_cache_hit_increments_cache_hits() {
        let state = RizState::new();
        state.register(FunctionState::user("GET /api", make_route_config())).await;
        state.record_invocation("GET /api", 1.0, true, true).await;
        let functions = state.functions.read().await;
        let f = functions.get("GET /api").unwrap();
        assert_eq!(f.cache_hits.load(Ordering::Relaxed), 1);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn unhealthy_invocation_increments_errors_and_flips_healthy() {
        let state = RizState::new();
        state.register(FunctionState::user("GET /api", make_route_config())).await;
        state.record_invocation("GET /api", 1.0, false, false).await;
        let functions = state.functions.read().await;
        let f = functions.get("GET /api").unwrap();
        assert_eq!(f.errors.load(Ordering::Relaxed), 1);
        assert!(!f.healthy.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn iter_preserves_registration_order() {
        let state = RizState::new();
        state.register(FunctionState::user("GET /b", make_route_config())).await;
        state.register(FunctionState::user("GET /a", make_route_config())).await;
        state.register(FunctionState::system("GET /_riz/health")).await;
        let functions = state.functions.read().await;
        let keys: Vec<&str> = functions.keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["GET /b", "GET /a", "GET /_riz/health"]);
    }

    fn make_route_config() -> crate::config::RouteConfig {
        crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./handler.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test riz_state 2>&1 | tail -10
```

Expected: compile errors.

- [ ] **Step 3: Implement `FunctionState`, `FunctionKind`, `RizState`**

Add to `src/state.rs` (after `LatencyWindow`):

```rust
use indexmap::IndexMap;
use crate::config::RouteConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    User,
    System,
}

/// Per-function runtime state. Counters are atomic; latency is mutex-guarded
/// because percentile computation needs the full sample window.
pub struct FunctionState {
    pub route_key: String,
    pub route: Option<RouteConfig>,
    pub kind: FunctionKind,
    pub invocations: AtomicU64,
    pub errors: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub cold_starts: AtomicU64,
    pub healthy: AtomicBool,
    pub last_invoked: std::sync::Mutex<Option<Instant>>,
    pub latency: std::sync::Mutex<LatencyWindow>,
}

impl FunctionState {
    pub fn user(route_key: impl Into<String>, route: RouteConfig) -> Self {
        Self {
            route_key: route_key.into(),
            route: Some(route),
            kind: FunctionKind::User,
            invocations: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            cold_starts: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
            last_invoked: std::sync::Mutex::new(None),
            latency: std::sync::Mutex::new(LatencyWindow::new()),
        }
    }

    pub fn system(route_key: impl Into<String>) -> Self {
        Self {
            route_key: route_key.into(),
            route: None,
            kind: FunctionKind::System,
            invocations: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            cold_starts: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
            last_invoked: std::sync::Mutex::new(None),
            latency: std::sync::Mutex::new(LatencyWindow::new()),
        }
    }
}

/// Shared runtime state — replaces the old AppState.route_stats map.
pub struct RizState {
    pub functions: tokio::sync::RwLock<IndexMap<String, Arc<FunctionState>>>,
    pub start_time: Instant,
    pub version: &'static str,
}

impl RizState {
    pub fn new() -> Self {
        Self {
            functions: tokio::sync::RwLock::new(IndexMap::new()),
            start_time: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub async fn register(&self, f: FunctionState) {
        let mut functions = self.functions.write().await;
        functions.insert(f.route_key.clone(), Arc::new(f));
    }

    /// Hot-path bookkeeping. Read-locks the outer map, atomic-bumps the entry,
    /// briefly Mutex-locks latency to push one sample.
    pub async fn record_invocation(
        &self,
        route_key: &str,
        latency_ms: f64,
        healthy: bool,
        cache_hit: bool,
    ) {
        let functions = self.functions.read().await;
        let Some(entry) = functions.get(route_key) else { return };
        entry.invocations.fetch_add(1, Ordering::Relaxed);
        if cache_hit {
            entry.cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.cache_misses.fetch_add(1, Ordering::Relaxed);
        }
        if !healthy {
            entry.errors.fetch_add(1, Ordering::Relaxed);
        }
        entry.healthy.store(healthy, Ordering::Relaxed);
        if let Ok(mut last) = entry.last_invoked.lock() {
            *last = Some(Instant::now());
        }
        if let Ok(mut w) = entry.latency.lock() {
            w.push(Instant::now(), latency_ms);
        }
    }

    pub async fn note_cold_start(&self, route_key: &str) {
        let functions = self.functions.read().await;
        if let Some(entry) = functions.get(route_key) {
            entry.cold_starts.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }
}

impl Default for RizState {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test riz_state 2>&1 | tail -10
```

Expected: 6 passed, 0 failed.

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: existing 143 tests + new ones still pass.

- [ ] **Step 6: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): add RizState + FunctionState with cold-start tracking"
```

---

## Task 5: Define LambdaHandler trait + HandlerError

**Files:**
- Modify: `src/runtime/mod.rs`

- [ ] **Step 1: Write failing tests for trait surface**

Append to `src/runtime/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_method_matches_any() {
        assert!(RouteMethod::Any.matches("GET"));
        assert!(RouteMethod::Any.matches("POST"));
        assert!(RouteMethod::Any.matches("PUT"));
    }

    #[test]
    fn route_method_matches_specific() {
        assert!(RouteMethod::Get.matches("GET"));
        assert!(RouteMethod::Get.matches("get"));
        assert!(!RouteMethod::Get.matches("POST"));
        assert!(RouteMethod::Post.matches("POST"));
    }

    #[test]
    fn route_method_from_str_parses_common_verbs() {
        assert_eq!(RouteMethod::from_str("GET"), RouteMethod::Get);
        assert_eq!(RouteMethod::from_str("get"), RouteMethod::Get);
        assert_eq!(RouteMethod::from_str("PATCH"), RouteMethod::Patch);
        assert_eq!(RouteMethod::from_str("ANY"), RouteMethod::Any);
        assert_eq!(RouteMethod::from_str("UNKNOWN"), RouteMethod::Any);
    }

    #[test]
    fn route_entry_matches_exact_path() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/api".into() };
        assert!(e.matches("GET", "/api"));
        assert!(!e.matches("POST", "/api"));
        assert!(!e.matches("GET", "/api/users"));
    }

    #[test]
    fn handler_error_status_codes() {
        assert_eq!(HandlerError::Timeout(30).status_code(), 504);
        assert_eq!(HandlerError::Overloaded(10).status_code(), 429);
        assert_eq!(HandlerError::Process("died".into()).status_code(), 502);
        assert_eq!(HandlerError::InvalidResponse("bad json".into()).status_code(), 500);
        assert_eq!(HandlerError::Internal("x".into()).status_code(), 500);
    }

    #[test]
    fn handler_error_to_response_has_json_body() {
        let err = HandlerError::Timeout(30);
        let resp = err.to_response();
        assert_eq!(resp.status_code, 504);
        let body = resp.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["message"].as_str().unwrap().contains("timeout"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib runtime:: 2>&1 | tail -10
```

Expected: compile errors.

- [ ] **Step 3: Implement trait, types, and error enum**

Replace contents of `src/runtime/mod.rs`:

```rust
//! Riz runtime — LambdaHandler trait and the canonical request/response types.
//! All handlers (user functions and system functions) implement LambdaHandler.

pub mod process;

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use crate::gateway::{GatewayRequest, GatewayResponse};

#[async_trait]
pub trait LambdaHandler: Send + Sync {
    /// Stable name for logs and registry display.
    fn name(&self) -> &str;

    /// Routes this handler serves. Each is checked against the incoming request;
    /// the router picks the first handler whose RouteEntry matches.
    fn routes(&self) -> &[RouteEntry];

    /// Optional: synchronous shutdown hook (e.g. kill child processes).
    /// Default: no-op.
    fn on_shutdown(&self) {}

    /// Process one event. Returns Ok(response) on success, Err for runtime
    /// failures (which the router converts to a 5xx response).
    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: RouteMethod,
    pub path: String,
}

impl RouteEntry {
    pub fn matches(&self, method: &str, path: &str) -> bool {
        self.method.matches(method) && self.path == path
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMethod {
    Any,
    Get, Post, Put, Delete, Patch, Head, Options,
}

impl RouteMethod {
    pub fn matches(&self, method: &str) -> bool {
        match self {
            RouteMethod::Any => true,
            other => method.eq_ignore_ascii_case(other.as_str()),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RouteMethod::Any => "ANY",
            RouteMethod::Get => "GET",
            RouteMethod::Post => "POST",
            RouteMethod::Put => "PUT",
            RouteMethod::Delete => "DELETE",
            RouteMethod::Patch => "PATCH",
            RouteMethod::Head => "HEAD",
            RouteMethod::Options => "OPTIONS",
        }
    }

    /// Permissive parse: unknown verbs map to Any. Used at construction time
    /// where the verb came from `riz.toml`.
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "GET" => RouteMethod::Get,
            "POST" => RouteMethod::Post,
            "PUT" => RouteMethod::Put,
            "DELETE" => RouteMethod::Delete,
            "PATCH" => RouteMethod::Patch,
            "HEAD" => RouteMethod::Head,
            "OPTIONS" => RouteMethod::Options,
            _ => RouteMethod::Any,
        }
    }
}

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
    pub fn status_code(&self) -> u16 {
        match self {
            HandlerError::Timeout(_) => 504,
            HandlerError::Overloaded(_) => 429,
            HandlerError::Process(_) => 502,
            HandlerError::InvalidResponse(_) => 500,
            HandlerError::Internal(_) => 500,
        }
    }

    pub fn to_response(&self) -> GatewayResponse {
        #[derive(Serialize)]
        struct Body<'a> { message: &'a str }
        let body = serde_json::to_string(&Body { message: &self.to_string() }).unwrap();
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        GatewayResponse {
            status_code: self.status_code(),
            headers: Some(headers),
            body: Some(body),
            is_base64_encoded: None,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --lib runtime:: 2>&1 | tail -10
```

Expected: 6 passed.

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: all tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/runtime/mod.rs
git commit -m "feat(runtime): add LambdaHandler trait + RouteEntry + HandlerError"
```

---

## Task 6: Refactor Router to dispatch via LambdaHandler trait

**Files:**
- Modify: `src/router.rs` (entire file)

This task replaces the router's data model. The router now holds `Vec<Arc<dyn LambdaHandler>>` and dispatches by iterating handlers.

- [ ] **Step 1: Write failing tests for the new Router API**

Replace the existing `#[cfg(test)] mod tests` block in `src/router.rs` with these tests, keeping the existing helper tests for `percent_decode`/`hex_val`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
    use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct StubHandler {
        name: String,
        routes: Vec<RouteEntry>,
        body: String,
    }

    #[async_trait]
    impl LambdaHandler for StubHandler {
        fn name(&self) -> &str { &self.name }
        fn routes(&self) -> &[RouteEntry] { &self.routes }
        async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
            Ok(GatewayResponse {
                status_code: 200,
                headers: None,
                body: Some(self.body.clone()),
                is_base64_encoded: None,
            })
        }
    }

    fn make_event(method: &str, path: &str) -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: format!("{method} {path}"),
            raw_path: path.into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: method.into(),
                    path: path.into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "req-1".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
        }
    }

    #[test]
    fn route_key_format_preserved() {
        assert_eq!(Router::route_key("get", "/api"), "GET /api");
    }

    #[tokio::test]
    async fn first_matching_handler_wins() {
        let h1 = Arc::new(StubHandler {
            name: "first".into(),
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "from-first".into(),
        });
        let h2 = Arc::new(StubHandler {
            name: "second".into(),
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "from-second".into(),
        });
        let router = Router::new(vec![h1, h2]);
        let resp = router.dispatch(make_event("GET", "/api")).await.unwrap();
        assert_eq!(resp.body.as_deref(), Some("from-first"));
    }

    #[tokio::test]
    async fn no_match_returns_404_handler_error_response() {
        let router = Router::new(vec![]);
        let resp = router.dispatch(make_event("GET", "/no-such")).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn method_mismatch_returns_404() {
        let h = Arc::new(StubHandler {
            name: "only-get".into(),
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "x".into(),
        });
        let router = Router::new(vec![h]);
        let resp = router.dispatch(make_event("POST", "/api")).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn route_method_any_matches_all_methods() {
        let h = Arc::new(StubHandler {
            name: "any".into(),
            routes: vec![RouteEntry { method: RouteMethod::Any, path: "/api".into() }],
            body: "ok".into(),
        });
        let router = Router::new(vec![h]);
        for m in &["GET", "POST", "PUT", "DELETE", "PATCH"] {
            let resp = router.dispatch(make_event(m, "/api")).await.unwrap();
            assert_eq!(resp.status_code, 200, "method {m} should match");
        }
    }

    #[test]
    fn percent_decode_passthrough_unencoded() {
        assert_eq!(percent_decode("normal"), "normal");
    }

    #[test]
    fn percent_decode_handles_encoded_slash() {
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
    }

    #[test]
    fn percent_decode_handles_space() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
    }

    #[test]
    fn percent_decode_mixed() {
        assert_eq!(percent_decode("foo%2Fbar/baz"), "foo/bar/baz");
    }
}
```

- [ ] **Step 2: Run failing tests**

```bash
cargo test --lib router:: 2>&1 | tail -10
```

Expected: compile errors.

- [ ] **Step 3: Replace `Router` implementation**

Replace the existing `pub struct Router` and `impl Router` in `src/router.rs` with:

```rust
use std::sync::Arc;
use crate::gateway::GatewayRequest;
use crate::runtime::{HandlerError, LambdaHandler};

pub struct Router {
    handlers: Vec<Arc<dyn LambdaHandler>>,
}

impl Router {
    pub fn new(handlers: Vec<Arc<dyn LambdaHandler>>) -> Self {
        Self { handlers }
    }

    pub fn empty() -> Self {
        Self { handlers: Vec::new() }
    }

    /// Stable key format used in logs/metrics/registry.
    pub fn route_key(method: &str, pattern: &str) -> String {
        format!("{} {}", method.to_uppercase(), pattern)
    }

    pub fn handlers(&self) -> &[Arc<dyn LambdaHandler>] {
        &self.handlers
    }

    /// Dispatch one event through the first matching handler. Returns 404 if
    /// no handler claims the route.
    pub async fn dispatch(
        &self,
        event: GatewayRequest,
    ) -> Result<crate::gateway::GatewayResponse, HandlerError> {
        let method = event.request_context.http.method.as_str();
        let path = event.request_context.http.path.as_str();
        for h in &self.handlers {
            for r in h.routes() {
                if r.matches(method, path) {
                    return h.invoke(event).await;
                }
            }
        }
        Ok(crate::gateway::GatewayResponse::error(404, "not found"))
    }
}
```

Remove the old `RouteMatch` struct, `match_pattern` function, and `match_route` method — they are no longer used.

Keep `percent_decode` and `hex_val` as they are still needed for path parameter handling (now done by individual handlers, but the helper functions remain available for future use).

- [ ] **Step 4: The router refactor breaks compilation across the codebase**

`src/server.rs`, `src/hotreload.rs`, `src/main.rs`, `src/deploy.rs`, and `tests/*` all reference `Router::new(config.routes.clone())` (passing `Vec<RouteConfig>`). The signature is now `Router::new(Vec<Arc<dyn LambdaHandler>>)`. Update each caller:

In `src/main.rs`: replace `let router = router::Router::new(config.routes.clone());` with a temporary `let router = router::Router::empty();` — the actual handler mounting happens in Task 8.

In `src/hotreload.rs`: find any `Router::new(...)` calls — replace with `Router::empty()` for now. The hot-reload diff logic will be rewired in a later step.

In `tests/integration_test.rs`: same — `Router::empty()`.

In `tests/http_boundary.rs`: same — `Router::empty()`.

The signature `Router::route_key` is unchanged so other call sites still work.

- [ ] **Step 5: Update `dispatch_lambda` to use the new Router**

Edit `src/server.rs` — the `dispatch_lambda` function. It currently uses `router.match_route(&method, &path)`. Replace with `router.dispatch(gw_request).await`. Keep all the surrounding logic (cache check, response recording, log push, etc.).

Replace the `match router.match_route(&method, &path)` block plus the entire process_manager.invoke call below it with this flow:

```rust
// Cache check (unchanged)
if let Some(cached) = state.cache.get(&cache_key).await {
    // ... existing cached-hit logic ...
    return gateway_to_axum(&cached);
}

// Build event (unchanged from existing code: extract_headers, body, etc.)
let gw_request = GatewayRequest { /* ... existing fields ... */ };

// NEW: dispatch through trait router instead of process_manager.invoke
let router = state.router.read().await;
let result = router.dispatch(gw_request.clone()).await;
drop(router);

let latency = start.elapsed().as_secs_f64() * 1000.0;
let route_key = Router::route_key(&method, &path);

match result {
    Ok(gw_resp) => {
        let healthy = gw_resp.status_code < 500;
        state.metrics.record_request(&route_key, &method, gw_resp.status_code, latency);
        // ... rest of existing post-dispatch logic ...
    }
    Err(e) => {
        let resp = e.to_response();
        // record error in metrics and state
        gateway_to_axum(&resp)
    }
}
```

The exact replacement: open `src/server.rs`, find the `dispatch_lambda` function (around line 86), and rewrite its body to follow the structure above. Keep the request-building code, cache check, headers extraction, body extraction, and log/metrics recording. Replace only the route-matching and invocation parts.

For now, the route_key derivation switches from "matched route's path pattern" to "the incoming path" — since the new router doesn't return the matched RouteEntry. This is a known limitation that Spec B will fix when routes support patterns; for Spec A, all routes are exact-path so this is correct.

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: the router-level tests pass. The integration tests and http_boundary tests may have some failures because the router is now empty (no ProcessHandler yet). The `health`/`ready`/`deploy`/`cache/invalidate` endpoints still work (they're separate axum routes in `build_app`).

If the unknown-path test fails because router.dispatch returns 404 but the fallback was previously a raw `(StatusCode::NOT_FOUND, "not found")`, that's fine — both are 404.

- [ ] **Step 7: Commit**

```bash
git add src/router.rs src/server.rs src/main.rs src/hotreload.rs tests/
git commit -m "refactor(router): dispatch via LambdaHandler trait"
```

---

## Task 7: Implement ProcessHandler

**Files:**
- Modify: `src/runtime/process.rs`
- Modify: `src/process/mod.rs` (export RoutePool + spawn_process; keep ProcessManager as a thin convenience wrapper for tests)

- [ ] **Step 1: Make pool internals available to ProcessHandler**

Open `src/process/mod.rs`. The current `ProcessManager` owns a `RwLock<HashMap<String, Arc<RoutePool>>>`. For `ProcessHandler` to own a single pool, the `RoutePool` struct and the `spawn_process` function need to be `pub(crate)` (already may be — verify).

Look near the top of `src/process/mod.rs` for the definition of `RoutePool`. Ensure it's `pub(crate) struct RoutePool { ... }` and that the fields needed (`semaphore`, `processes`, `healthy`, `restart_count`, `consecutive_crashes`, `runtime_registry`, `route`, `log_tx`) are accessible. Also ensure `spawn_process`, `handle_process_failure`, and `kill_process_group` are `pub(crate)`.

If they aren't already, change `pub fn` / `fn` to `pub(crate) fn` / `pub(crate) struct` as needed.

- [ ] **Step 2: Write failing tests for ProcessHandler**

Create the test block in `src/runtime/process.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_handler_exposes_single_route_entry() {
        let route = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        // We can construct the handler without spawning by using a no-op builder.
        let h = ProcessHandler::new_stub_for_tests(route.clone());
        assert_eq!(h.routes().len(), 1);
        assert_eq!(h.routes()[0].path, "/api");
        assert_eq!(h.name(), "GET /api");
    }
}
```

- [ ] **Step 3: Run failing tests**

```bash
cargo test --lib runtime::process 2>&1 | tail -10
```

Expected: compile errors.

- [ ] **Step 4: Implement ProcessHandler**

Replace contents of `src/runtime/process.rs`:

```rust
//! ProcessHandler — owns one route's process pool and implements LambdaHandler.
//! Ports the existing ProcessManager.invoke flow into a per-route trait impl.

use async_trait::async_trait;
use std::sync::Arc;
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::RoutePool;
use crate::process::runtime::RuntimeRegistry;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{LogEntry, RizState};

pub struct ProcessHandler {
    name: String,
    routes: Vec<RouteEntry>,
    pool: Arc<RoutePool>,
    riz_state: Arc<RizState>,
    route_key: String,
}

impl ProcessHandler {
    /// Spawn the pool for `route` and wrap it as a LambdaHandler.
    /// Increments cold_starts for the initial pool members via RizState.
    pub async fn spawn(
        route: RouteConfig,
        registry: Arc<RuntimeRegistry>,
        log_tx: tokio::sync::mpsc::Sender<LogEntry>,
        riz_state: Arc<RizState>,
    ) -> anyhow::Result<Self> {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        let pool = crate::process::spawn_pool(route.clone(), registry, log_tx).await?;
        // Each successful initial spawn is a cold start.
        for _ in 0..route.concurrency.max(1) {
            riz_state.note_cold_start(&route_key).await;
        }
        let method = RouteMethod::from_str(&route.method);
        Ok(Self {
            name: route_key.clone(),
            routes: vec![RouteEntry { method, path: route.path.clone() }],
            pool,
            riz_state,
            route_key,
        })
    }

    /// Construct without spawning. Tests only — uses a stub pool guarded by
    /// `cfg(test)`. Not exposed in release builds.
    #[cfg(test)]
    pub fn new_stub_for_tests(route: RouteConfig) -> Self {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        let method = RouteMethod::from_str(&route.method);
        Self {
            name: route_key.clone(),
            routes: vec![RouteEntry { method, path: route.path.clone() }],
            pool: crate::process::stub_pool_for_tests(route.clone()),
            riz_state: Arc::new(RizState::new()),
            route_key,
        }
    }

    pub fn route_key(&self) -> &str {
        &self.route_key
    }

    pub fn pool(&self) -> &Arc<RoutePool> {
        &self.pool
    }
}

#[async_trait]
impl LambdaHandler for ProcessHandler {
    fn name(&self) -> &str { &self.name }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        // Acquire semaphore — try, don't wait. Existing behavior.
        let timeout_ms = self.pool.route.timeout_ms;
        let resp = crate::process::pool_invoke(&self.pool, &event, timeout_ms)
            .await
            .map_err(|e| {
                // pool_invoke uses anyhow::Error today; translate to HandlerError
                let msg = e.to_string();
                if msg.contains("timeout") {
                    HandlerError::Timeout(timeout_ms)
                } else if msg.contains("overloaded") || msg.contains("no permits") {
                    HandlerError::Overloaded(self.pool.route.concurrency as usize)
                } else if msg.contains("invalid response") {
                    HandlerError::InvalidResponse(msg)
                } else {
                    HandlerError::Process(msg)
                }
            })?;
        Ok(resp)
    }

    fn on_shutdown(&self) {
        let stats = futures::executor::block_on(self.pool.stats());
        for &pid in &stats.pids {
            crate::process::kill_process_group(pid);
        }
    }
}
```

- [ ] **Step 5: Expose the necessary helpers in `src/process/mod.rs`**

In `src/process/mod.rs`, add public(crate) free functions that the new `ProcessHandler` calls:

```rust
/// Spawn a single route's pool. Returns the populated pool.
pub(crate) async fn spawn_pool(
    route: crate::config::RouteConfig,
    registry: Arc<crate::process::runtime::RuntimeRegistry>,
    log_tx: tokio::sync::mpsc::Sender<crate::state::LogEntry>,
) -> anyhow::Result<Arc<RoutePool>> {
    // Extract the pool-creation logic from ProcessManager::spawn_all
    // and adapt for one route.
    // ... see existing ProcessManager::spawn_all body ...
    let pool = Arc::new(RoutePool::new(route, registry, log_tx));
    pool.populate().await?;
    pool.start_liveness_watchers();
    Ok(pool)
}

/// Invoke against a specific pool — moves the invoke logic out of ProcessManager.
pub(crate) async fn pool_invoke(
    pool: &Arc<RoutePool>,
    event: &crate::gateway::GatewayRequest,
    timeout_ms: u64,
) -> anyhow::Result<crate::gateway::GatewayResponse> {
    // Extract invoke body from ProcessManager::invoke.
    // ... existing logic ...
    pool.invoke(event, timeout_ms).await
}

#[cfg(test)]
pub(crate) fn stub_pool_for_tests(route: crate::config::RouteConfig) -> Arc<RoutePool> {
    Arc::new(RoutePool::stub_for_tests(route))
}
```

The implementer should look at the existing `ProcessManager::spawn_all` and `ProcessManager::invoke` (in `src/process/mod.rs`) and lift their bodies into these helpers. If `RoutePool` doesn't yet have `populate()`, `start_liveness_watchers()`, `stats()`, or `stub_for_tests()` methods, add them by extracting those bodies from the current `ProcessManager` methods.

**ProcessManager itself remains** as a thin convenience used by `ready_handler`, `kill_all_processes`, and tests — it can keep its `pool_stats()`, `hot_swap()`, and `spawn_all()` methods backed by the same primitives. Don't delete it; just make sure its `invoke()` is no longer the dispatch entry point (the router is now).

- [ ] **Step 6: Run tests**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: process_handler tests pass; existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/runtime/process.rs src/process/mod.rs
git commit -m "feat(runtime): ProcessHandler implementing LambdaHandler"
```

---

## Task 8: Mount ProcessHandlers in main.rs; wire up router

**Files:**
- Modify: `src/main.rs`
- Modify: `src/state.rs` — add `riz_state: Arc<RizState>` to AppState

- [ ] **Step 1: Add `riz_state` to AppState**

In `src/state.rs`, find the `pub struct AppState {...}` and add a field:

```rust
pub riz_state: Arc<RizState>,
```

Also add a top-of-file import if not already present:
```rust
// (Arc and RizState are already in scope)
```

Remove the `route_stats: RwLock<HashMap<String, Arc<RouteStats>>>` field — it's replaced by `riz_state`. The existing `RouteStats`/`RouteStatsSnapshot` types remain in `state.rs` for now (the TUI still uses them); they will be removed in Task 12 after the TUI is updated.

Actually — to minimize Task 8's blast radius, keep `route_stats` AND add `riz_state`. The TUI keeps reading `route_stats`. We'll dual-write to both for the bridge period. Remove `route_stats` in Task 12 after the TUI migration.

- [ ] **Step 2: Update `record_request` in AppState to ALSO write to RizState**

In `src/state.rs`, find the existing `pub async fn record_request(...)` on `AppState`. After the existing atomics update, append:

```rust
// Dual-write to RizState while the TUI still reads route_stats.
self.riz_state.record_invocation(route_key, latency_ms, healthy, cache_hit).await;
```

- [ ] **Step 3: In `src/main.rs`, build the handler list**

Find the `main()` function in `src/main.rs`. After loading config but before constructing AppState:

```rust
let riz_state = Arc::new(state::RizState::new());

// Register each user function in RizState
for route in &config.routes {
    let route_key = router::Router::route_key(&route.method, &route.path);
    riz_state.register(state::FunctionState::user(route_key, route.clone())).await;
}

// Build user handlers: one ProcessHandler per route
let mut handlers: Vec<std::sync::Arc<dyn runtime::LambdaHandler>> = Vec::new();
for route in &config.routes {
    let h = runtime::process::ProcessHandler::spawn(
        route.clone(),
        registry.clone(),
        log_tx.clone(),
        riz_state.clone(),
    ).await?;
    handlers.push(std::sync::Arc::new(h));
}

let router = router::Router::new(handlers);
```

Remove the older `process_manager.spawn_all(...)` call — it's now redundant because each `ProcessHandler::spawn` populates its own pool.

But `process_manager` is still used by `ready_handler`, `pool_stats`, and `kill_all_processes`. To bridge: keep `ProcessManager::new()` (empty), and don't call `spawn_all`. Update `ready_handler` and `kill_all_processes` to iterate `app_state.router.read().await.handlers()` and downcast or use a method to extract pool PIDs and health. 

Simpler: add a method to AppState that iterates handlers and reports unhealthy ones:

```rust
impl AppState {
    pub async fn unhealthy_handlers(&self) -> Vec<String> {
        let router = self.router.read().await;
        let mut out = Vec::new();
        for h in router.handlers() {
            // ProcessHandler can be detected by downcasting; alternatively, 
            // we check riz_state.functions[name].healthy
            let functions = self.riz_state.functions.read().await;
            if let Some(f) = functions.get(h.name()) {
                if !f.healthy.load(std::sync::atomic::Ordering::Relaxed) {
                    out.push(h.name().to_string());
                }
            }
        }
        out
    }

    pub async fn shutdown_all_handlers(&self) {
        let router = self.router.read().await;
        for h in router.handlers() {
            h.on_shutdown();
        }
    }
}
```

Update `ready_handler` in `src/server.rs` to call `state.unhealthy_handlers().await`. Update `kill_all_processes` to call `state.shutdown_all_handlers().await`.

- [ ] **Step 4: Pass riz_state to AppState constructor**

In `src/main.rs`, when constructing AppState:

```rust
let app_state = Arc::new(state::AppState {
    config: tokio::sync::RwLock::new(config.clone()),
    router: tokio::sync::RwLock::new(router),
    process_manager,      // kept but empty
    cache,
    metrics,
    runtime_registry: registry,
    route_stats: tokio::sync::RwLock::new(Default::default()), // bridge
    log_tx,
    log_rx: tokio::sync::Mutex::new(log_rx),
    riz_state,
});
```

- [ ] **Step 5: Build**

```bash
cargo build 2>&1 | tail -10
```

Fix any compile errors. The main classes of errors to expect:
- `Router::new` signature changed — callers in `tests/integration_test.rs` and `tests/http_boundary.rs` need updating.
- `AppState` literal needs `riz_state`.

Update each.

For `tests/integration_test.rs`: after constructing `process_manager` and `riz_state`, build handlers the same way main.rs does, then `Router::new(handlers)`. Mirror main.rs's setup.

For `tests/http_boundary.rs`: routes are empty (`vec![]`), so handlers list is empty, so `Router::empty()` is fine.

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: tests still pass. The Layer 1 golden tests in `tests/http_boundary.rs` keep their original behavior.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/state.rs src/server.rs tests/
git commit -m "refactor(main): mount ProcessHandlers; expose riz_state to AppState"
```

---

## Task 9: HealthHandler

**Files:**
- Modify: `src/system/health.rs`

- [ ] **Step 1: Write failing tests for HealthHandler**

Replace contents of `src/system/health.rs`:

```rust
//! /_riz/health handler — returns 200 with runtime + per-function status.

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
    functions: Vec<FunctionHealth>,
}

#[derive(Serialize)]
struct FunctionHealth {
    route_key: String,
    healthy: bool,
    invocations: u64,
    errors: u64,
    p50_ms: f64,
    p99_ms: f64,
    last_invoked_secs_ago: Option<f64>,
}

pub struct HealthHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl HealthHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/health".into() }],
            riz_state,
        }
    }
}

#[async_trait]
impl LambdaHandler for HealthHandler {
    fn name(&self) -> &str { "GET /_riz/health" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<FunctionHealth> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) {
                // System functions excluded from health body to avoid noise.
                continue;
            }
            let (p50, _, _, _, p99) = f.latency.lock()
                .map(|mut w| w.percentiles(now))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
            let last_secs = f.last_invoked.lock()
                .ok()
                .and_then(|l| l.map(|t| now.duration_since(t).as_secs_f64()));
            out.push(FunctionHealth {
                route_key: f.route_key.clone(),
                healthy: f.healthy.load(Ordering::Relaxed),
                invocations: f.invocations.load(Ordering::Relaxed),
                errors: f.errors.load(Ordering::Relaxed),
                p50_ms: p50,
                p99_ms: p99,
                last_invoked_secs_ago: last_secs,
            });
        }
        let body = HealthBody {
            status: "ok",
            version: self.riz_state.version,
            uptime_secs: self.riz_state.uptime_secs(),
            functions: out,
        };
        let json = serde_json::to_string(&body)
            .map_err(|e| HandlerError::Internal(e.to_string()))?;
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        Ok(GatewayResponse {
            status_code: 200,
            headers: Some(headers),
            body: Some(json),
            is_base64_encoded: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;

    fn make_event() -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /_riz/health".into(),
            raw_path: "/_riz/health".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: crate::gateway::RequestContext {
                http: crate::gateway::HttpContext {
                    method: "GET".into(),
                    path: "/_riz/health".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
        }
    }

    fn make_user_state() -> FunctionState {
        let route = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./h.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", route)
    }

    #[tokio::test]
    async fn health_returns_200_with_ok_status() {
        let s = Arc::new(RizState::new());
        let h = HealthHandler::new(s);
        let resp = h.invoke(make_event()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
        assert!(body["functions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn health_includes_registered_user_functions() {
        let s = Arc::new(RizState::new());
        s.register(make_user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(make_event()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0]["route_key"], "GET /api");
        assert_eq!(functions[0]["healthy"], true);
    }

    #[tokio::test]
    async fn health_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        s.register(make_user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(make_event()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1, "system functions must be excluded");
        assert_eq!(functions[0]["route_key"], "GET /api");
    }

    #[tokio::test]
    async fn health_reflects_recorded_invocations() {
        let s = Arc::new(RizState::new());
        s.register(make_user_state()).await;
        s.record_invocation("GET /api", 10.0, true, false).await;
        s.record_invocation("GET /api", 20.0, true, false).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(make_event()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["functions"][0]["invocations"], 2);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib system::health 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/system/health.rs
git commit -m "feat(system): HealthHandler for /_riz/health"
```

---

## Task 10: MetricsHandler (Prometheus text)

**Files:**
- Modify: `src/system/metrics.rs`

- [ ] **Step 1: Write failing tests**

Replace contents of `src/system/metrics.rs`:

```rust
//! /_riz/metrics handler — emits Prometheus text format 0.0.4.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::fmt::Write;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

pub struct MetricsHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl MetricsHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/metrics".into() }],
            riz_state,
        }
    }
}

fn esc(s: &str) -> String {
    // Prometheus label value escaping
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

#[async_trait]
impl LambdaHandler for MetricsHandler {
    fn name(&self) -> &str { "GET /_riz/metrics" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out = String::with_capacity(4096);

        writeln!(out, "# HELP riz_invocations_total Total function invocations").unwrap();
        writeln!(out, "# TYPE riz_invocations_total counter").unwrap();
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.invocations.load(Ordering::Relaxed);
            writeln!(out, "riz_invocations_total{{route=\"{}\"}} {}", esc(&f.route_key), n).unwrap();
        }

        writeln!(out, "# HELP riz_errors_total Total function errors").unwrap();
        writeln!(out, "# TYPE riz_errors_total counter").unwrap();
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.errors.load(Ordering::Relaxed);
            writeln!(out, "riz_errors_total{{route=\"{}\"}} {}", esc(&f.route_key), n).unwrap();
        }

        writeln!(out, "# HELP riz_latency_ms Function latency percentiles over 5-min window").unwrap();
        writeln!(out, "# TYPE riz_latency_ms summary").unwrap();
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let (p50, p75, p90, p95, p99) = f.latency.lock()
                .map(|mut w| w.percentiles(now))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
            let route = esc(&f.route_key);
            writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.5\"}} {}", route, p50).unwrap();
            writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.75\"}} {}", route, p75).unwrap();
            writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.9\"}} {}", route, p90).unwrap();
            writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.95\"}} {}", route, p95).unwrap();
            writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.99\"}} {}", route, p99).unwrap();
        }

        writeln!(out, "# HELP riz_cold_starts_total Process spawns").unwrap();
        writeln!(out, "# TYPE riz_cold_starts_total counter").unwrap();
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.cold_starts.load(Ordering::Relaxed);
            writeln!(out, "riz_cold_starts_total{{route=\"{}\"}} {}", esc(&f.route_key), n).unwrap();
        }

        writeln!(out, "# HELP riz_function_healthy 1 if pool healthy, 0 otherwise").unwrap();
        writeln!(out, "# TYPE riz_function_healthy gauge").unwrap();
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let v = if f.healthy.load(Ordering::Relaxed) { 1 } else { 0 };
            writeln!(out, "riz_function_healthy{{route=\"{}\"}} {}", esc(&f.route_key), v).unwrap();
        }

        writeln!(out, "# HELP riz_uptime_seconds Runtime uptime").unwrap();
        writeln!(out, "# TYPE riz_uptime_seconds gauge").unwrap();
        writeln!(out, "riz_uptime_seconds {}", self.riz_state.uptime_secs()).unwrap();

        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "text/plain; version=0.0.4".into());
        Ok(GatewayResponse {
            status_code: 200,
            headers: Some(headers),
            body: Some(out),
            is_base64_encoded: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;

    fn evt() -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /_riz/metrics".into(),
            raw_path: "/_riz/metrics".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: crate::gateway::RequestContext {
                http: crate::gateway::HttpContext {
                    method: "GET".into(),
                    path: "/_riz/metrics".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
        }
    }

    fn user_state() -> FunctionState {
        let r = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./h.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_text() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let ct = resp.headers.unwrap().get("content-type").unwrap().clone();
        assert!(ct.starts_with("text/plain; version=0.0.4"));
    }

    #[tokio::test]
    async fn metrics_emits_help_and_type_lines() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(body.contains("# HELP riz_invocations_total"));
        assert!(body.contains("# TYPE riz_invocations_total counter"));
        assert!(body.contains("# TYPE riz_latency_ms summary"));
        assert!(body.contains("riz_uptime_seconds"));
    }

    #[tokio::test]
    async fn metrics_includes_user_function_counters() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        s.record_invocation("GET /api", 5.0, true, false).await;
        s.record_invocation("GET /api", 10.0, false, false).await;
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(body.contains("riz_invocations_total{route=\"GET /api\"} 2"), "body was:\n{body}");
        assert!(body.contains("riz_errors_total{route=\"GET /api\"} 1"));
    }

    #[tokio::test]
    async fn metrics_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(!body.contains("/_riz/health"), "system functions must not appear in metrics");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib system::metrics 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/system/metrics.rs
git commit -m "feat(system): MetricsHandler emitting Prometheus text"
```

---

## Task 11: RegistryHandler (JSON manifest)

**Files:**
- Modify: `src/system/registry.rs`

- [ ] **Step 1: Write tests + implement**

Replace contents of `src/system/registry.rs`:

```rust
//! /_riz/registry handler — JSON manifest of all mounted routes (user + system).

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

pub struct RegistryHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl RegistryHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/registry".into() }],
            riz_state,
        }
    }
}

#[derive(Serialize)]
struct RegistryBody {
    version: &'static str,
    functions: Vec<RegistryFunction>,
}

#[derive(Serialize)]
struct RegistryFunction {
    route_key: String,
    method: String,
    path: String,
    runtime: Option<String>,
    kind: &'static str,
    handler: Option<String>,
    timeout_ms: Option<u64>,
    concurrency: Option<u32>,
    cache_ttl_secs: Option<u64>,
}

#[async_trait]
impl LambdaHandler for RegistryHandler {
    fn name(&self) -> &str { "GET /_riz/registry" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<RegistryFunction> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            let (runtime, handler, timeout_ms, concurrency, cache_ttl_secs) = match &f.route {
                Some(r) => (
                    Some(r.runtime.as_str().to_string()),
                    Some(r.handler.to_string_lossy().to_string()),
                    Some(r.timeout_ms),
                    Some(r.concurrency),
                    r.cache_ttl_secs,
                ),
                None => (None, None, None, None, None),
            };
            let kind = match f.kind {
                FunctionKind::User => "user",
                FunctionKind::System => "system",
            };
            // Extract method and path from route_key "METHOD /path"
            let (method, path) = match f.route_key.split_once(' ') {
                Some((m, p)) => (m.to_string(), p.to_string()),
                None => (String::new(), f.route_key.clone()),
            };
            out.push(RegistryFunction {
                route_key: f.route_key.clone(),
                method,
                path,
                runtime,
                kind,
                handler,
                timeout_ms,
                concurrency,
                cache_ttl_secs,
            });
        }
        let body = RegistryBody { version: self.riz_state.version, functions: out };
        let json = serde_json::to_string(&body)
            .map_err(|e| HandlerError::Internal(e.to_string()))?;
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        Ok(GatewayResponse {
            status_code: 200,
            headers: Some(headers),
            body: Some(json),
            is_base64_encoded: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;

    fn evt() -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /_riz/registry".into(),
            raw_path: "/_riz/registry".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: crate::gateway::RequestContext {
                http: crate::gateway::HttpContext {
                    method: "GET".into(),
                    path: "/_riz/registry".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
        }
    }

    fn user_state() -> FunctionState {
        let r = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 3,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn registry_returns_json_with_version() {
        let s = Arc::new(RizState::new());
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert!(body["version"].is_string());
        assert!(body["functions"].is_array());
    }

    #[tokio::test]
    async fn registry_lists_user_functions_with_full_fields() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "user");
        assert_eq!(f["method"], "GET");
        assert_eq!(f["path"], "/api");
        assert_eq!(f["runtime"], "bun");
        assert_eq!(f["timeout_ms"], 5000);
        assert_eq!(f["concurrency"], 3);
    }

    #[tokio::test]
    async fn registry_lists_system_functions_with_nulls() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "system");
        assert_eq!(f["method"], "GET");
        assert_eq!(f["path"], "/_riz/health");
        assert!(f["runtime"].is_null());
        assert!(f["handler"].is_null());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib system::registry 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/system/registry.rs
git commit -m "feat(system): RegistryHandler for /_riz/registry"
```

---

## Task 12: McpHandler (JSON-RPC tools/list + tools/call)

**Files:**
- Modify: `src/system/mcp.rs`
- Modify: `src/system/mod.rs` (add `pub fn mcp_tool_name` helper)

- [ ] **Step 1: Add `mcp_tool_name` helper to `src/system/mod.rs`**

```rust
//! Riz system functions mounted under /_riz/*.

pub mod health;
pub mod metrics;
pub mod registry;
pub mod mcp;

/// Derive a stable, MCP-compatible tool name from a route_key like "GET /api/users/:id".
/// Result: "GET_api_users_id".
pub fn mcp_tool_name(route_key: &str) -> String {
    let mut out = String::with_capacity(route_key.len());
    for c in route_key.chars() {
        match c {
            ' ' | '/' => out.push('_'),
            ':' => continue,
            _ => out.push(c),
        }
    }
    // Trim trailing/leading underscores
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_strips_colon_replaces_slash() {
        assert_eq!(mcp_tool_name("GET /api"), "GET_api");
        assert_eq!(mcp_tool_name("POST /accounts/:id"), "POST_accounts_id");
        assert_eq!(mcp_tool_name("GET /a/b/c"), "GET_a_b_c");
    }
}
```

- [ ] **Step 2: Implement `McpHandler`**

Replace contents of `src/system/mcp.rs`:

```rust
//! /_riz/mcp handler — JSON-RPC 2.0 implementing MCP tools/list + tools/call.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
use crate::router::Router;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};
use crate::system::mcp_tool_name;

pub struct McpHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
    router: tokio::sync::RwLock<Option<Arc<Router>>>,
}

impl McpHandler {
    /// `router` is set after Router construction via `set_router`. We can't
    /// pass it at construction time because the Router itself contains this
    /// handler — chicken-and-egg.
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Post, path: "/_riz/mcp".into() }],
            riz_state,
            router: tokio::sync::RwLock::new(None),
        }
    }

    pub async fn set_router(&self, router: Arc<Router>) {
        *self.router.write().await = Some(router);
    }
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcOk<T: Serialize> {
    jsonrpc: &'static str,
    id: serde_json::Value,
    result: T,
}

#[derive(Serialize)]
struct JsonRpcErr {
    jsonrpc: &'static str,
    id: serde_json::Value,
    error: JsonRpcErrBody,
}

#[derive(Serialize)]
struct JsonRpcErrBody {
    code: i32,
    message: String,
}

#[derive(Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

#[derive(Serialize)]
struct ToolsListResult {
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct ToolsCallResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Serialize)]
struct ToolContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Deserialize)]
struct ToolsCallParams {
    name: String,
    #[serde(default)]
    arguments: ToolArguments,
}

#[derive(Deserialize, Default)]
struct ToolArguments {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default, rename = "queryParams")]
    query_params: HashMap<String, String>,
    #[serde(default, rename = "pathParams")]
    path_params: HashMap<String, String>,
    #[serde(default, rename = "isBase64Encoded")]
    is_base64_encoded: bool,
}

#[async_trait]
impl LambdaHandler for McpHandler {
    fn name(&self) -> &str { "POST /_riz/mcp" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let body = event.body.as_deref().unwrap_or("{}");
        let req: JsonRpcRequest = match serde_json::from_str(body) {
            Ok(r) => r,
            Err(e) => return Ok(jsonrpc_error(serde_json::Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        match req.method.as_str() {
            "tools/list" => self.tools_list(id).await,
            "tools/call" => self.tools_call(id, req.params).await,
            other => Ok(jsonrpc_error(id, -32601, &format!("method not found: {other}"))),
        }
    }
}

impl McpHandler {
    async fn tools_list(&self, id: serde_json::Value) -> Result<GatewayResponse, HandlerError> {
        let functions = self.riz_state.functions.read().await;
        let mut tools = Vec::new();
        for (_, f) in functions.iter() {
            if !matches!(f.kind, FunctionKind::User) { continue; }
            let name = mcp_tool_name(&f.route_key);
            let description = match &f.route {
                Some(r) => format!("Invoke {} ({} runtime)", f.route_key, r.runtime.as_str()),
                None => format!("Invoke {}", f.route_key),
            };
            tools.push(Tool {
                name,
                description,
                input_schema: generic_envelope_schema(),
            });
        }
        let result = ToolsListResult { tools };
        ok_response(id, result)
    }

    async fn tools_call(&self, id: serde_json::Value, params: serde_json::Value) -> Result<GatewayResponse, HandlerError> {
        let parsed: ToolsCallParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => return Ok(jsonrpc_error(id, -32602, &format!("invalid params: {e}"))),
        };

        // Find the route_key matching the tool name
        let functions = self.riz_state.functions.read().await;
        let mut matched: Option<(String, String, String)> = None;  // (route_key, method, path)
        for (route_key, f) in functions.iter() {
            if !matches!(f.kind, FunctionKind::User) { continue; }
            if mcp_tool_name(route_key) == parsed.name {
                if let Some((m, p)) = route_key.split_once(' ') {
                    matched = Some((route_key.clone(), m.to_string(), p.to_string()));
                    break;
                }
            }
        }
        drop(functions);

        let (route_key, method, path) = match matched {
            Some(m) => m,
            None => return Ok(jsonrpc_error(id, -32602, &format!("unknown tool: {}", parsed.name))),
        };

        // Assemble GatewayRequest
        let raw_qs = parsed.query_params.iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let event = GatewayRequest {
            version: "2.0".into(),
            route_key: route_key.clone(),
            raw_path: path.clone(),
            raw_query_string: raw_qs,
            headers: parsed.headers,
            request_context: RequestContext {
                http: HttpContext {
                    method: method.clone(),
                    path: path.clone(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: uuid::Uuid::new_v4().to_string(),
                time_epoch: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            },
            path_parameters: if parsed.path_params.is_empty() { None } else { Some(parsed.path_params) },
            body: parsed.body,
            is_base64_encoded: parsed.is_base64_encoded,
        };

        // Reentrant dispatch
        let router = self.router.read().await;
        let router = match router.as_ref() {
            Some(r) => r.clone(),
            None => return Ok(jsonrpc_error(id, -32603, "router not initialized")),
        };
        let inner = match router.dispatch(event).await {
            Ok(r) => r,
            Err(e) => e.to_response(),
        };

        let is_error = inner.status_code >= 400;
        let inner_json = serde_json::to_string(&inner)
            .map_err(|e| HandlerError::Internal(e.to_string()))?;
        let result = ToolsCallResult {
            content: vec![ToolContent { kind: "text", text: inner_json }],
            is_error,
        };
        ok_response(id, result)
    }
}

fn generic_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "body": {"type": "string", "description": "Request body. Set isBase64Encoded:true for binary."},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}},
            "queryParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "pathParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "isBase64Encoded": {"type": "boolean", "default": false}
        }
    })
}

fn urlencode(s: &str) -> String {
    // Minimal URL-encode for the bytes that matter (space, =, &)
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            other => out.push_str(&format!("%{:02X}", other as u32)),
        }
    }
    out
}

fn ok_response<T: Serialize>(id: serde_json::Value, result: T) -> Result<GatewayResponse, HandlerError> {
    let body = JsonRpcOk { jsonrpc: "2.0", id, result };
    let json = serde_json::to_string(&body)
        .map_err(|e| HandlerError::Internal(e.to_string()))?;
    let mut headers = HashMap::new();
    headers.insert("content-type".into(), "application/json".into());
    Ok(GatewayResponse {
        status_code: 200,
        headers: Some(headers),
        body: Some(json),
        is_base64_encoded: None,
    })
}

fn jsonrpc_error(id: serde_json::Value, code: i32, message: &str) -> GatewayResponse {
    let body = JsonRpcErr {
        jsonrpc: "2.0",
        id,
        error: JsonRpcErrBody { code, message: message.to_string() },
    };
    let json = serde_json::to_string(&body).unwrap_or_else(|_| String::from("{}"));
    let mut headers = HashMap::new();
    headers.insert("content-type".into(), "application/json".into());
    GatewayResponse {
        status_code: 200,  // JSON-RPC errors travel as 200 with error body
        headers: Some(headers),
        body: Some(json),
        is_base64_encoded: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;

    fn evt(body: &str) -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "POST /_riz/mcp".into(),
            raw_path: "/_riz/mcp".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "POST".into(),
                    path: "/_riz/mcp".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: Some(body.to_string()),
            is_base64_encoded: false,
        }
    }

    fn user_state() -> FunctionState {
        let r = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn tools_list_returns_user_functions_as_tools() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "GET_api");
        assert!(tools[0]["description"].as_str().unwrap().contains("GET /api"));
    }

    #[tokio::test]
    async fn tools_list_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "GET_api");
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = "not json";
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_with_missing_router_returns_internal_error() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        // Don't set_router
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"GET_api","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32603);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        h.set_router(Arc::new(Router::empty())).await;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"GET_nope","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32602);
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib system::mcp 2>&1 | tail -10
```

Expected: 6 passed.

- [ ] **Step 4: Commit**

```bash
git add src/system/mcp.rs src/system/mod.rs
git commit -m "feat(system): McpHandler with tools/list + tools/call"
```

---

## Task 13: Mount system handlers; reject /_riz/ prefix at config load

**Files:**
- Modify: `src/config.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing test for config validation**

Append to the `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
#[test]
fn config_rejects_reserved_riz_prefix() {
    let toml_str = r#"
[server]
port = 8080
host = "127.0.0.1"

[[routes]]
path = "/_riz/health"
method = "GET"
runtime = "bun"
handler = "./h.ts"
timeout_ms = 1000
concurrency = 1
"#;
    let result = toml::from_str::<Config>(toml_str).and_then(|c| {
        c.validate().map(|()| c).map_err(|e| toml::de::Error::custom(e.to_string()))
    });
    assert!(result.is_err(), "config with /_riz/ route must be rejected");
}

#[test]
fn config_validate_accepts_normal_routes() {
    let toml_str = r#"
[server]
port = 8080
host = "127.0.0.1"

[[routes]]
path = "/api"
method = "GET"
runtime = "bun"
handler = "./h.ts"
timeout_ms = 1000
concurrency = 1
"#;
    let c: Config = toml::from_str(toml_str).unwrap();
    assert!(c.validate().is_ok());
}
```

- [ ] **Step 2: Add `validate()` to Config**

Add this method on `impl Config` in `src/config.rs`:

```rust
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        for r in &self.routes {
            if r.path.starts_with("/_riz/") || r.path == "/_riz" {
                return Err(format!("route path '{}' uses reserved /_riz/ prefix", r.path));
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Call `validate()` in main**

In `src/main.rs`, after loading config:

```rust
config.validate().map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;
```

- [ ] **Step 4: Mount system handlers first in main.rs**

In `src/main.rs`, replace the handler-building block from Task 8 with:

```rust
let riz_state = Arc::new(state::RizState::new());

// Register system functions in state
riz_state.register(state::FunctionState::system("GET /_riz/health")).await;
riz_state.register(state::FunctionState::system("GET /_riz/metrics")).await;
riz_state.register(state::FunctionState::system("GET /_riz/registry")).await;
riz_state.register(state::FunctionState::system("POST /_riz/mcp")).await;

// Register user functions
for route in &config.routes {
    let route_key = router::Router::route_key(&route.method, &route.path);
    riz_state.register(state::FunctionState::user(route_key, route.clone())).await;
}

// Build handler list — system handlers FIRST so they shadow any user attempt
let mcp = Arc::new(system::mcp::McpHandler::new(riz_state.clone()));
let mut handlers: Vec<Arc<dyn runtime::LambdaHandler>> = vec![
    Arc::new(system::health::HealthHandler::new(riz_state.clone())),
    Arc::new(system::metrics::MetricsHandler::new(riz_state.clone())),
    Arc::new(system::registry::RegistryHandler::new(riz_state.clone())),
    mcp.clone() as Arc<dyn runtime::LambdaHandler>,
];

// User functions next, in toml order
for route in &config.routes {
    let h = runtime::process::ProcessHandler::spawn(
        route.clone(),
        registry.clone(),
        log_tx.clone(),
        riz_state.clone(),
    ).await?;
    handlers.push(Arc::new(h));
}

let router_inner = Arc::new(router::Router::new(handlers));
mcp.set_router(router_inner.clone()).await;

// Wrap in RwLock for AppState consistency with current API
let app_router = tokio::sync::RwLock::new(router::Router::new(router_inner.handlers().to_vec()));
```

This needs a small adjustment to the Router type — `Router::new` needs to accept `Vec<Arc<dyn LambdaHandler>>` (it already does). The double-construction with `router_inner` is needed because McpHandler needs an `Arc<Router>` reference before AppState wraps the Router in RwLock.

Alternative cleaner approach: make `AppState.router` hold `Arc<Router>` directly (no RwLock) — the Router is immutable once constructed for Spec A. This requires touching `src/hotreload.rs` (which currently `router.write().await`'s).

For Spec A: leave the RwLock in place but construct the inner Arc<Router> first, hand to MCP, then wrap a clone in RwLock. Hot-reload-driven router updates are out of scope for Spec A.

- [ ] **Step 5: Build and run tests**

```bash
cargo build 2>&1 | tail -10
cargo test 2>&1 | grep "test result"
```

Expected: builds. All tests pass. The HTTP boundary tests in `tests/http_boundary.rs` from Task 1 still pass — `health`, `ready`, `deploy`, `cache/invalidate` all behave as before.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: mount system handlers first; reject /_riz/ prefix in config"
```

---

## Task 14: System functions integration tests

**Files:**
- Create: `tests/system_functions_integration.rs`

- [ ] **Step 1: Write integration tests**

Create `tests/system_functions_integration.rs`:

```rust
//! Layer 3 — full server integration. Spins up the server with a stub user
//! function and verifies each system endpoint reflects expected state.

use std::net::SocketAddr;
use std::sync::Arc;

async fn make_state() -> Arc<riz::state::AppState> {
    let mut config = riz::config::Config::default();
    config.routes = vec![];  // no real lambdas; we'll inject a fake state entry
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let process_manager = riz::process::ProcessManager::new();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    riz_state.register(riz::state::FunctionState::system("GET /_riz/health")).await;
    riz_state.register(riz::state::FunctionState::system("GET /_riz/metrics")).await;
    riz_state.register(riz::state::FunctionState::system("GET /_riz/registry")).await;
    riz_state.register(riz::state::FunctionState::system("POST /_riz/mcp")).await;

    // Synthetic user function (no real process)
    let route = riz::config::RouteConfig {
        path: "/echo".into(),
        method: "GET".into(),
        runtime: riz::config::RuntimeKind::Bun,
        handler: std::path::PathBuf::from("./echo.ts"),
        timeout_ms: 5000,
        cache_ttl_secs: None,
        concurrency: 1,
    };
    riz_state.register(riz::state::FunctionState::user("GET /echo", route)).await;
    // Pre-record an invocation so health/metrics have non-zero values
    riz_state.record_invocation("GET /echo", 12.5, true, false).await;

    let mcp = std::sync::Arc::new(riz::system::mcp::McpHandler::new(riz_state.clone()));
    let handlers: Vec<std::sync::Arc<dyn riz::runtime::LambdaHandler>> = vec![
        std::sync::Arc::new(riz::system::health::HealthHandler::new(riz_state.clone())),
        std::sync::Arc::new(riz::system::metrics::MetricsHandler::new(riz_state.clone())),
        std::sync::Arc::new(riz::system::registry::RegistryHandler::new(riz_state.clone())),
        mcp.clone() as std::sync::Arc<dyn riz::runtime::LambdaHandler>,
    ];
    let router_inner = std::sync::Arc::new(riz::router::Router::new(handlers.clone()));
    mcp.set_router(router_inner.clone()).await;

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(handlers)),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
    })
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn health_endpoint_reports_user_function() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    let functions = body["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["route_key"] == "GET /echo"));
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/metrics")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("text/plain"));
    let body = resp.text().await.unwrap();
    assert!(body.contains("riz_invocations_total{route=\"GET /echo\"} 1"), "{body}");
    assert!(body.contains("riz_uptime_seconds"));
}

#[tokio::test]
async fn registry_endpoint_lists_user_and_system_functions() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/registry")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let functions = body["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["kind"] == "system" && f["path"] == "/_riz/health"));
    assert!(functions.iter().any(|f| f["kind"] == "user" && f["path"] == "/echo"));
}

#[tokio::test]
async fn mcp_tools_list_includes_user_function() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let resp = client.post(format!("http://{addr}/_riz/mcp"))
        .json(&req).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "GET_echo"));
}

#[tokio::test]
async fn mcp_unknown_method_returns_jsonrpc_error() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"nope"});
    let resp = client.post(format!("http://{addr}/_riz/mcp"))
        .json(&req).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601);
}
```

- [ ] **Step 2: Run integration tests**

```bash
cargo test --test system_functions_integration 2>&1 | tail -10
```

Expected: 5 passed.

- [ ] **Step 3: Run the full suite**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: all suites pass — lib + bin + http_boundary + integration_test + system_functions_integration.

- [ ] **Step 4: Commit**

```bash
git add tests/system_functions_integration.rs
git commit -m "test: integration coverage for /_riz/* endpoints"
```

---

## Task 15: Final verification + cleanup

- [ ] **Step 1: Run the entire test suite**

```bash
cargo test 2>&1 | tail -20
```

Expected: every test passes across every binary/lib/integration target. The 6 tests from `tests/http_boundary.rs` (the drift-prevention layer) must all be green.

- [ ] **Step 2: Verify the binary still starts and serves**

Build the release binary and confirm it boots:

```bash
cargo build --release 2>&1 | tail -3
```

- [ ] **Step 3: Smoke test against a running instance (optional but recommended)**

```bash
./target/release/riz --config examples/riz.dev.toml --no-tui &
sleep 1
curl -s localhost:8080/_riz/health | head -c 200
curl -s localhost:8080/_riz/registry | head -c 200
curl -s -X POST localhost:8080/_riz/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | head -c 200
kill %1
```

Each should return a JSON or Prometheus response. The mcp tools/list should include any user function from `examples/riz.dev.toml`.

- [ ] **Step 4: Commit any final cleanup if needed (no commit if no changes)**

```bash
git status
```

If clean, no commit needed. If `route_stats` is fully unused after the refactor, remove it:

In `src/state.rs`: delete the `route_stats` field from `AppState`, delete `RouteStats` and `RouteStatsSnapshot` if no callers remain. In `src/tui/`, update to read from `riz_state.functions` instead.

If the TUI migration is non-trivial, leave `route_stats` for now and create a follow-up bug to remove it later.

```bash
git add -u
git commit -m "chore: cleanup unused route_stats after RizState migration" || true
```

- [ ] **Step 5: Done. Report final test counts.**

```bash
cargo test 2>&1 | grep "test result"
```

Capture the test count and pass that back as the final report.
