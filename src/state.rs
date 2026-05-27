use crate::cache::CacheLayer;
use crate::config::Config;
use crate::metrics::MetricsEmitter;
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
    pub metrics: MetricsEmitter,
    pub runtime_registry: Arc<RuntimeRegistry>,
    pub log_tx: mpsc::Sender<LogEntry>,
    pub log_rx: Mutex<mpsc::Receiver<LogEntry>>,
    pub riz_state: Arc<RizState>,
    pub ws_connections: crate::ws::ConnectionStore,
}

#[derive(Clone)]
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

    pub fn count(&mut self, now: Instant) -> usize {
        self.evict_stale(now);
        self.samples.len()
    }

    /// Raw sample count including stale entries — for tests and MAX_SAMPLES checks.
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

    pub fn user(name: impl Into<String>, config: FunctionConfig) -> Self {
        let name = name.into();
        let routes = config
            .effective_routes(&name)
            .into_iter()
            .map(|r| format!("{} {}", r.method.to_uppercase(), r.path))
            .collect();
        Self {
            name,
            routes,
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

    pub fn system(name: impl Into<String>, routes: Vec<String>) -> Self {
        Self {
            name: name.into(),
            routes,
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
}

/// Shared runtime state. Single source of truth for all per-function metrics.
pub struct RizState {
    pub functions: RwLock<IndexMap<String, Arc<FunctionState>>>,
    pub start_time: Instant,
    pub version: &'static str,
}

impl RizState {
    pub fn new() -> Self {
        Self {
            functions: RwLock::new(IndexMap::new()),
            start_time: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
        }
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
        }
    }

    #[tokio::test]
    async fn function_state_starts_zeroed() {
        let f = FunctionState::user("api", make_function_config());
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
        let f = FunctionState::system("_riz/health", vec!["GET /_riz/health".into()]);
        assert_eq!(f.kind, FunctionKind::System);
        assert!(f.config.is_none());
    }

    #[tokio::test]
    async fn register_then_record_increments_counters() {
        let state = RizState::new();
        state
            .register(FunctionState::user("api", make_function_config()))
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
            .register(FunctionState::user("api", make_function_config()))
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
            .register(FunctionState::user("api", make_function_config()))
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
            .register(FunctionState::user("b", make_function_config()))
            .await;
        state
            .register(FunctionState::user("a", make_function_config()))
            .await;
        state
            .register(FunctionState::system(
                "_riz_health",
                vec!["GET /_riz/health".into()],
            ))
            .await;
        let functions = state.functions.read().await;
        let keys: Vec<&str> = functions.keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["b", "a", "_riz_health"]);
    }
}
