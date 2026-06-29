use crate::auth::authorizer::AuthCache;
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::observability::TelemetryHandle;
use crate::process::runtime::RuntimeRegistry;
use crate::process::ProcessManager;
use crate::router::Router;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, Mutex, RwLock};

pub struct AppState {
    pub config: RwLock<Config>,
    pub router: RwLock<Router>,
    pub process_manager: Arc<ProcessManager>,
    pub cache: CacheLayer,
    /// Authorizer response cache (keyed by source_ip + auth_header_hash + function_name).
    pub auth_cache: AuthCache,
    /// Non-blocking, best-effort telemetry emitter (OTLP/HTTP-JSON span export
    /// via the isolated `__telemetry` child). `emit` never blocks the request
    /// path; a disabled handle drops every event. See `observability::`.
    pub telemetry: TelemetryHandle,
    pub runtime_registry: Arc<RuntimeRegistry>,
    pub log_tx: mpsc::Sender<LogEntry>,
    pub log_rx: Mutex<mpsc::Receiver<LogEntry>>,
    pub riz_state: Arc<RizState>,
    pub ws_connections: crate::ws::ConnectionStore,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub level: String,
    pub message: String,
    pub route_key: Option<String>,
}

impl AppState {
    pub fn push_log(&self, level: &str, route_key: Option<&str>, message: String) {
        let _ = self.log_tx.try_send(LogEntry {
            timestamp: SystemTime::now(),
            level: level.into(),
            message,
            route_key: route_key.map(|s| s.to_string()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_channel_applies_backpressure() {
        // Verify that try_send on a full bounded channel returns an error
        // (proving the send site won't OOM — it drops the entry instead)
        let (tx, _rx) = tokio::sync::mpsc::channel::<i32>(2);
        tx.try_send(1).unwrap();
        tx.try_send(2).unwrap();
        assert!(tx.try_send(3).is_err(), "full channel must reject send");
    }

    #[test]
    fn log_entry_has_route_key_field() {
        let entry = LogEntry {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: "INFO".into(),
            message: "test".into(),
            route_key: Some("GET /ping".into()),
        };
        assert_eq!(entry.route_key.as_deref(), Some("GET /ping"));

        let global = LogEntry {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: "WARN".into(),
            message: "system".into(),
            route_key: None,
        };
        assert!(global.route_key.is_none());
    }
}

// ─── LatencyWindow ──────────────────────────────────────────────────────────

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
        Self {
            samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, now: Instant, latency_ms: f64) {
        if self.samples.len() >= Self::MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((now, latency_ms));
    }

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

    /// Returns (p50, p75, p90, p95, p99) over the live window using nearest-rank.
    /// Empty window returns zeros.
    pub fn percentiles(&mut self, now: Instant) -> (f64, f64, f64, f64, f64) {
        self.evict_stale(now);
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0, 0.0, 0.0);
        }
        let mut sorted: Vec<f64> = self.samples.iter().map(|&(_, v)| v).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        let q = |p: f64| -> f64 {
            let idx = ((p * n as f64).ceil() as usize)
                .saturating_sub(1)
                .min(n - 1);
            sorted[idx]
        };
        (q(0.50), q(0.75), q(0.90), q(0.95), q(0.99))
    }

    #[allow(dead_code)]
    pub fn count(&mut self, now: Instant) -> usize {
        self.evict_stale(now);
        self.samples.len()
    }

    /// Raw sample count including stale entries — for tests and MAX_SAMPLES checks.
    #[allow(dead_code)]
    pub fn raw_len(&self) -> usize {
        self.samples.len()
    }
}

impl Default for LatencyWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod latency_window_tests {
    use super::*;

    #[test]
    fn empty_window_returns_zero_percentiles() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        assert_eq!(w.percentiles(now), (0.0, 0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn latency_window_emits_all_percentiles() {
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
        assert!(p50 <= p75);
        assert!(p75 <= p90);
        assert!(p90 <= p95);
        assert!(p95 <= p99);
    }

    #[test]
    fn linear_distribution_gives_expected_percentiles() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for i in 1..=100 {
            w.push(now, i as f64);
        }
        let (p50, _, _, p95, p99) = w.percentiles(now);
        assert!((p50 - 50.0).abs() < 1.0, "p50={p50}");
        assert!((p95 - 95.0).abs() < 1.0, "p95={p95}");
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

// ─── FunctionState + RizState ──────────────────────────────────────────────

use crate::config::FunctionConfig;
use indexmap::IndexMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FunctionKind {
    #[default]
    User,
    System,
    /// A WASM guard pool (`{fn}::guard_in` / `{fn}::guard_out`). Visible in
    /// /_riz/health and /_riz/registry (guard timing is an acceptance
    /// criterion), but never an MCP tool and not a routable function.
    Guard,
}

/// Per-function runtime state. One entry per FUNCTION (not per route) —
/// matches AWS Lambda's CloudWatch metric shape where counters and latency
/// percentiles aggregate at the function level regardless of how many routes
/// invoke it. Counters are atomic for lock-free hot path; latency uses a
/// Mutex because percentile computation needs the full sample window.
pub struct FunctionState {
    /// Function name (`api`, `users`, `_riz/health` for system).
    pub name: String,
    /// All routes this function serves, as "METHOD /path" strings for
    /// display in /_riz/health, /_riz/registry, and the TUI.
    pub routes: Vec<String>,
    /// Function-level config for user functions; None for system endpoints.
    pub config: Option<FunctionConfig>,
    pub kind: FunctionKind,
    // ── Cached per-function metadata (hot-path reads; never touch config lock) ──
    /// Effective cache TTL for this function: per-function override when set,
    /// otherwise the server-level default. Cached at registration so
    /// dispatch_lambda never needs to touch state.config.
    pub cache_ttl_secs: AtomicU64,
    /// API Gateway stage name (e.g. "$default", "prod"). Cached from
    /// server-level config at registration time.
    pub stage: std::sync::Mutex<String>,
    /// Handler timeout in milliseconds. Mirrors FunctionConfig::timeout_ms
    /// but lives here so the hot path never needs the config lock.
    pub timeout_ms: AtomicU64,
    /// Runtime tag string (e.g. "bun", "python", "rust", "system"). Used by
    /// metrics on the hot path without re-reading the config lock.
    pub runtime_tag: std::sync::Mutex<String>,
    // ── Counters and instrumentation ──────────────────────────────────────────
    pub invocations: AtomicU64,
    pub errors: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub cold_starts: AtomicU64,
    pub healthy: AtomicBool,
    pub last_invoked: std::sync::Mutex<Option<Instant>>,
    pub latency: std::sync::Mutex<LatencyWindow>,
}

/// Plain-data view of one FunctionState — what the TUI renders.
#[derive(Clone, Debug, Default)]
pub struct FunctionStateSnapshot {
    pub name: String,
    #[allow(dead_code)]
    pub routes: Vec<String>,
    pub kind: FunctionKind,
    pub invocations: u64,
    pub errors: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cold_starts: u64,
    pub healthy: bool,
    pub p50_ms: f64,
    pub p75_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    #[allow(dead_code)]
    pub last_invoked_secs_ago: Option<f64>,
}

impl FunctionStateSnapshot {
    pub fn hit_rate_pct(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64 * 100.0
        }
    }
}

impl FunctionState {
    /// Capture an immutable snapshot.
    pub fn snapshot(&self, now: Instant) -> FunctionStateSnapshot {
        let (p50, p75, p90, p95, p99) = self
            .latency
            .lock()
            .map(|mut w| w.percentiles(now))
            .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
        let last_invoked_secs_ago = self
            .last_invoked
            .lock()
            .ok()
            .and_then(|l| l.map(|t| now.duration_since(t).as_secs_f64()));
        FunctionStateSnapshot {
            name: self.name.clone(),
            routes: self.routes.clone(),
            kind: self.kind,
            invocations: self.invocations.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            cold_starts: self.cold_starts.load(Ordering::Relaxed),
            healthy: self.healthy.load(Ordering::Relaxed),
            p50_ms: p50,
            p75_ms: p75,
            p90_ms: p90,
            p95_ms: p95,
            p99_ms: p99,
            last_invoked_secs_ago,
        }
    }

    /// Construct a user-function state.
    ///
    /// `stage` — server-level API Gateway stage name (e.g. `"$default"`).
    /// `default_ttl_secs` — server-level cache TTL used when the function
    ///   doesn't declare its own `cache_ttl_secs`.
    pub fn user(
        name: impl Into<String>,
        config: FunctionConfig,
        stage: impl Into<String>,
        default_ttl_secs: u64,
    ) -> Self {
        let name = name.into();
        let routes = config
            .effective_routes(&name)
            .into_iter()
            .map(|r| format!("{} {}", r.method.to_uppercase(), r.path))
            .collect();
        let effective_ttl = config.cache_ttl_secs.unwrap_or(default_ttl_secs);
        let timeout_ms = config.timeout_ms;
        let runtime_tag = config.runtime.as_str().to_string();
        Self {
            name,
            routes,
            cache_ttl_secs: AtomicU64::new(effective_ttl),
            stage: std::sync::Mutex::new(stage.into()),
            timeout_ms: AtomicU64::new(timeout_ms),
            runtime_tag: std::sync::Mutex::new(runtime_tag),
            config: Some(config),
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

    /// Guard pools: like system entries (no config, no cache) but with the
    /// Guard kind so health/registry surface their timing.
    pub fn guard(name: impl Into<String>, stage: impl Into<String>) -> Self {
        let mut s = Self::system(name, vec![], stage);
        s.kind = FunctionKind::Guard;
        *s.runtime_tag.lock().unwrap() = "wasm-guard".to_string();
        s
    }

    /// Construct a system-function state (e.g. `_riz_health`).
    ///
    /// System functions have no per-function config, so `stage` comes from
    /// the server config and `cache_ttl_secs` is always 0.
    pub fn system(name: impl Into<String>, routes: Vec<String>, stage: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            routes,
            cache_ttl_secs: AtomicU64::new(0),
            stage: std::sync::Mutex::new(stage.into()),
            timeout_ms: AtomicU64::new(0),
            runtime_tag: std::sync::Mutex::new("system".to_string()),
            config: None,
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

    /// Update mutable metadata fields after a hot-reload.
    ///
    /// Preserves all counters and latency samples — only the config-derived
    /// fields are updated. Called by the hot-reload path whenever a function's
    /// config changes so that `cache_ttl_secs`, `timeout_ms`, `runtime_tag`,
    /// and `stage` stay current without requiring a restart.
    pub fn update_metadata(&self, new_config: &FunctionConfig, stage: &str, default_ttl_secs: u64) {
        let effective_ttl = new_config.cache_ttl_secs.unwrap_or(default_ttl_secs);
        self.cache_ttl_secs.store(effective_ttl, Ordering::Relaxed);
        self.timeout_ms
            .store(new_config.timeout_ms, Ordering::Relaxed);
        if let Ok(mut s) = self.stage.lock() {
            *s = stage.to_string();
        }
        if let Ok(mut r) = self.runtime_tag.lock() {
            *r = new_config.runtime.as_str().to_string();
        }
    }
}

// ─── TokenStats (local LLM token read-model for the --dev TUI) ─────────────

/// One recorded chat-completion's token utilization. Plain, cloneable data.
#[derive(Clone, Debug)]
pub struct TokenCall {
    pub model: String,
    pub provider: String,
    pub input: u32,
    pub output: u32,
    /// Wall-clock time the call completed. Carried for future "x s ago"
    /// rendering / sorting; not yet displayed.
    #[allow(dead_code)]
    pub at: SystemTime,
}

/// Local, export-independent token read-model. Lives on `RizState` so the
/// `--dev` TUI can surface per-call token utilization even when
/// `[telemetry].enabled = false` (the OTLP export pipeline is a separate sink).
///
/// LLM calls flow through the global `/_riz/v1/chat/completions` gateway, not
/// arbitrary riz-functions, so token accounting is global/per-model — there is
/// no per-FunctionState token counter. Cumulative totals are lock-free atomics;
/// the recent-call ring is a short Mutex critical section (try_lock, capped).
pub struct TokenStats {
    total_input: AtomicU64,
    total_output: AtomicU64,
    recent: std::sync::Mutex<VecDeque<TokenCall>>,
}

impl TokenStats {
    /// Keep the most-recent N chat-completions for the TUI's recent-calls list.
    pub const RECENT_CAP: usize = 20;

    pub fn new() -> Self {
        Self {
            total_input: AtomicU64::new(0),
            total_output: AtomicU64::new(0),
            recent: std::sync::Mutex::new(VecDeque::new()),
        }
    }

    /// Record one chat-completion's token usage. Lock-light and non-blocking:
    /// totals are atomic; the recent ring uses `try_lock` so a contended render
    /// never stalls the response path (a dropped recent-entry is acceptable —
    /// the cumulative totals are always exact).
    pub fn record(&self, model: &str, provider: &str, input: u32, output: u32) {
        self.total_input.fetch_add(input as u64, Ordering::Relaxed);
        self.total_output
            .fetch_add(output as u64, Ordering::Relaxed);
        if let Ok(mut ring) = self.recent.try_lock() {
            ring.push_back(TokenCall {
                model: model.to_string(),
                provider: provider.to_string(),
                input,
                output,
                at: SystemTime::now(),
            });
            while ring.len() > Self::RECENT_CAP {
                ring.pop_front();
            }
        }
    }

    /// Capture an immutable, cloneable snapshot for the TUI snapshotter.
    pub fn snapshot(&self) -> TokenStatsSnapshot {
        let recent = self
            .recent
            .lock()
            .map(|r| r.iter().cloned().collect())
            .unwrap_or_default();
        TokenStatsSnapshot {
            total_input: self.total_input.load(Ordering::Relaxed),
            total_output: self.total_output.load(Ordering::Relaxed),
            recent,
        }
    }
}

impl Default for TokenStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Plain-data view of `TokenStats` — what the TUI renders. `recent` is ordered
/// oldest→newest (the TUI shows the tail). Cloneable and `Default`.
#[derive(Clone, Debug, Default)]
pub struct TokenStatsSnapshot {
    pub total_input: u64,
    pub total_output: u64,
    pub recent: Vec<TokenCall>,
}

impl TokenStatsSnapshot {
    pub fn total(&self) -> u64 {
        self.total_input + self.total_output
    }
}

/// Shared runtime state. Single source of truth for all per-function metrics.
pub struct RizState {
    pub functions: RwLock<IndexMap<String, Arc<FunctionState>>>,
    pub start_time: Instant,
    pub version: &'static str,
    /// Global LLM token utilization read-model (per-model, not per-function).
    pub token_stats: TokenStats,
}

impl RizState {
    pub fn new() -> Self {
        Self {
            functions: RwLock::new(IndexMap::new()),
            start_time: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
            token_stats: TokenStats::new(),
        }
    }

    /// Record one chat-completion's token utilization into the local read-model.
    /// Called from the gateway chat-completion path alongside the OTLP span —
    /// same data, two sinks (export + TUI). Non-blocking; safe on the hot path.
    pub fn record_tokens(&self, model: &str, provider: &str, input: u32, output: u32) {
        self.token_stats.record(model, provider, input, output);
    }

    pub async fn register(&self, f: FunctionState) {
        let mut functions = self.functions.write().await;
        functions.insert(f.name.clone(), Arc::new(f));
    }

    /// Hot-path bookkeeping. Keyed by FUNCTION NAME (not route_key) —
    /// matches AWS CloudWatch metric aggregation.
    pub async fn record_invocation(
        &self,
        function_name: &str,
        latency_ms: f64,
        healthy: bool,
        cache_hit: bool,
    ) {
        let entry = {
            let functions = self.functions.read().await;
            match functions.get(function_name) {
                Some(e) => e.clone(),
                None => return,
            }
        };
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
        let now = Instant::now();
        if let Ok(mut last) = entry.last_invoked.lock() {
            *last = Some(now);
        };
        if let Ok(mut w) = entry.latency.lock() {
            w.push(now, latency_ms);
        };
    }

    pub async fn note_cold_start(&self, function_name: &str) {
        let entry = {
            let functions = self.functions.read().await;
            match functions.get(function_name) {
                Some(e) => e.clone(),
                None => return,
            }
        };
        entry.cold_starts.fetch_add(1, Ordering::Relaxed);
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

#[cfg(test)]
mod riz_state_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn make_function_config() -> crate::config::FunctionConfig {
        crate::config::FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./handler.ts"),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        }
    }

    #[tokio::test]
    async fn function_state_starts_zeroed() {
        let f = FunctionState::user("api", make_function_config(), "$default", 0);
        assert_eq!(f.invocations.load(Ordering::Relaxed), 0);
        assert_eq!(f.errors.load(Ordering::Relaxed), 0);
        assert_eq!(f.cache_hits.load(Ordering::Relaxed), 0);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 0);
        assert_eq!(f.cold_starts.load(Ordering::Relaxed), 0);
        assert!(f.healthy.load(Ordering::Relaxed));
        assert_eq!(f.kind, FunctionKind::User);
        assert!(f.config.is_some());
        // Implicit default route is /api at ANY
        assert_eq!(f.routes, vec!["ANY /api".to_string()]);
    }

    #[tokio::test]
    async fn system_function_state_has_no_config() {
        let f = FunctionState::system("_riz/health", vec!["GET /_riz/health".into()], "$default");
        assert_eq!(f.kind, FunctionKind::System);
        assert!(f.config.is_none());
    }

    #[tokio::test]
    async fn register_then_record_increments_counters() {
        let state = RizState::new();
        state
            .register(FunctionState::user(
                "api",
                make_function_config(),
                "$default",
                0,
            ))
            .await;
        state.record_invocation("api", 12.3, true, false).await;
        state.record_invocation("api", 7.5, true, false).await;
        let functions = state.functions.read().await;
        let f = functions.get("api").unwrap();
        assert_eq!(f.invocations.load(Ordering::Relaxed), 2);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn record_invocation_for_unknown_function_is_noop() {
        let state = RizState::new();
        state
            .record_invocation("never-registered", 5.0, true, false)
            .await;
    }

    #[tokio::test]
    async fn record_invocation_with_cache_hit_increments_cache_hits() {
        let state = RizState::new();
        state
            .register(FunctionState::user(
                "api",
                make_function_config(),
                "$default",
                0,
            ))
            .await;
        state.record_invocation("api", 1.0, true, true).await;
        let functions = state.functions.read().await;
        let f = functions.get("api").unwrap();
        assert_eq!(f.cache_hits.load(Ordering::Relaxed), 1);
        assert_eq!(f.cache_misses.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn unhealthy_invocation_increments_errors_and_flips_healthy() {
        let state = RizState::new();
        state
            .register(FunctionState::user(
                "api",
                make_function_config(),
                "$default",
                0,
            ))
            .await;
        state.record_invocation("api", 1.0, false, false).await;
        let functions = state.functions.read().await;
        let f = functions.get("api").unwrap();
        assert_eq!(f.errors.load(Ordering::Relaxed), 1);
        assert!(!f.healthy.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn iter_preserves_registration_order() {
        let state = RizState::new();
        state
            .register(FunctionState::user(
                "b",
                make_function_config(),
                "$default",
                0,
            ))
            .await;
        state
            .register(FunctionState::user(
                "a",
                make_function_config(),
                "$default",
                0,
            ))
            .await;
        state
            .register(FunctionState::system(
                "_riz_health",
                vec!["GET /_riz/health".into()],
                "$default",
            ))
            .await;
        let functions = state.functions.read().await;
        let keys: Vec<&str> = functions.keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["b", "a", "_riz_health"]);
    }

    #[tokio::test]
    async fn function_state_caches_metadata_at_registration() {
        let mut cfg = make_function_config();
        cfg.cache_ttl_secs = Some(42);
        cfg.timeout_ms = 9000;
        let f = FunctionState::user("api", cfg, "prod", 60);
        // Per-function TTL override takes precedence over default_ttl_secs.
        assert_eq!(f.cache_ttl_secs.load(Ordering::Relaxed), 42);
        assert_eq!(f.timeout_ms.load(Ordering::Relaxed), 9000);
        assert_eq!(f.stage.lock().unwrap().as_str(), "prod");
        assert_eq!(f.runtime_tag.lock().unwrap().as_str(), "bun");
    }

    #[tokio::test]
    async fn function_state_uses_default_ttl_when_no_override() {
        let f = FunctionState::user("api", make_function_config(), "$default", 30);
        // make_function_config sets cache_ttl_secs = None → falls back to default.
        assert_eq!(f.cache_ttl_secs.load(Ordering::Relaxed), 30);
    }

    #[tokio::test]
    async fn update_metadata_changes_fields_without_resetting_counters() {
        let mut cfg = make_function_config();
        cfg.cache_ttl_secs = Some(10);
        cfg.timeout_ms = 1000;
        let f = FunctionState::user("api", cfg, "$default", 0);
        // Simulate an invocation so counters are non-zero.
        f.invocations.store(7, Ordering::Relaxed);
        f.cache_hits.store(3, Ordering::Relaxed);

        let mut new_cfg = make_function_config();
        new_cfg.cache_ttl_secs = Some(99);
        new_cfg.timeout_ms = 5000;
        f.update_metadata(&new_cfg, "v2", 0);

        assert_eq!(f.cache_ttl_secs.load(Ordering::Relaxed), 99);
        assert_eq!(f.timeout_ms.load(Ordering::Relaxed), 5000);
        assert_eq!(f.stage.lock().unwrap().as_str(), "v2");
        // Counters must be preserved.
        assert_eq!(f.invocations.load(Ordering::Relaxed), 7);
        assert_eq!(f.cache_hits.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn record_tokens_accumulates_totals() {
        let state = RizState::new();
        state.record_tokens("gpt-4o", "openai", 100, 40);
        state.record_tokens("anthropic/claude", "anthropic", 200, 60);
        let snap = state.token_stats.snapshot();
        assert_eq!(snap.total_input, 300);
        assert_eq!(snap.total_output, 100);
        assert_eq!(snap.total(), 400);
    }

    #[test]
    fn record_tokens_keeps_recent_calls_in_order() {
        let state = RizState::new();
        state.record_tokens("m1", "p1", 1, 2);
        state.record_tokens("m2", "p2", 3, 4);
        let snap = state.token_stats.snapshot();
        assert_eq!(snap.recent.len(), 2);
        // Oldest→newest ordering (TUI renders the tail).
        assert_eq!(snap.recent[0].model, "m1");
        assert_eq!(snap.recent[0].provider, "p1");
        assert_eq!(snap.recent[0].input, 1);
        assert_eq!(snap.recent[0].output, 2);
        assert_eq!(snap.recent[1].model, "m2");
        assert_eq!(snap.recent[1].output, 4);
    }

    #[test]
    fn recent_calls_are_capped_but_totals_remain_exact() {
        let state = RizState::new();
        let n = TokenStats::RECENT_CAP + 5;
        for i in 0..n {
            state.record_tokens(&format!("m{i}"), "p", 1, 1);
        }
        let snap = state.token_stats.snapshot();
        // Ring is capped to RECENT_CAP, retaining the newest entries.
        assert_eq!(snap.recent.len(), TokenStats::RECENT_CAP);
        assert_eq!(snap.recent.last().unwrap().model, format!("m{}", n - 1));
        assert_eq!(
            snap.recent.first().unwrap().model,
            format!("m{}", n - TokenStats::RECENT_CAP)
        );
        // Cumulative totals count every call regardless of the ring cap.
        assert_eq!(snap.total_input, n as u64);
        assert_eq!(snap.total_output, n as u64);
    }

    #[test]
    fn fresh_token_stats_snapshot_is_empty() {
        let snap = RizState::new().token_stats.snapshot();
        assert_eq!(snap.total_input, 0);
        assert_eq!(snap.total_output, 0);
        assert_eq!(snap.total(), 0);
        assert!(snap.recent.is_empty());
    }

    #[tokio::test]
    async fn system_function_state_has_zeroed_ttl_and_system_runtime_tag() {
        let f = FunctionState::system("_riz_metrics", vec!["GET /_riz/metrics".into()], "prod");
        assert_eq!(f.cache_ttl_secs.load(Ordering::Relaxed), 0);
        assert_eq!(f.timeout_ms.load(Ordering::Relaxed), 0);
        assert_eq!(f.runtime_tag.lock().unwrap().as_str(), "system");
        assert_eq!(f.stage.lock().unwrap().as_str(), "prod");
    }
}
