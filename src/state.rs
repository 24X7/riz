use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, Mutex, RwLock};
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::metrics::MetricsEmitter;
use crate::process::ProcessManager;
use crate::process::runtime::RuntimeRegistry;
use crate::router::Router;

pub struct AppState {
    pub config: RwLock<Config>,
    pub router: RwLock<Router>,
    pub process_manager: ProcessManager,
    pub cache: CacheLayer,
    pub metrics: MetricsEmitter,
    pub runtime_registry: Arc<RuntimeRegistry>,
    pub route_stats: RwLock<HashMap<String, Arc<RouteStats>>>,
    pub log_tx: mpsc::Sender<LogEntry>,
    pub log_rx: Mutex<mpsc::Receiver<LogEntry>>,
}

/// Per-route counters stored as atomics so the hot path only needs a READ lock
/// on the outer HashMap (to find the Arc), not a write lock.
pub struct RouteStats {
    pub total_requests: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub error_count: AtomicU64,
    /// Sum of all request latencies in microseconds.
    pub total_latency_us: AtomicU64,
    pub healthy: AtomicBool,
}

impl Default for RouteStats {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
        }
    }
}

impl RouteStats {
    /// Produce a plain-data snapshot for display/testing purposes.
    pub fn snapshot(&self) -> RouteStatsSnapshot {
        RouteStatsSnapshot {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            error_count: self.error_count.load(Ordering::Relaxed),
            total_latency_us: self.total_latency_us.load(Ordering::Relaxed),
            healthy: self.healthy.load(Ordering::Relaxed),
        }
    }
}

/// Plain-data snapshot of [`RouteStats`] suitable for the TUI and tests.
#[derive(Default, Clone)]
pub struct RouteStatsSnapshot {
    pub total_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub error_count: u64,
    pub total_latency_us: u64,
    pub healthy: bool,
}

impl RouteStatsSnapshot {
    /// Average latency in milliseconds (used as p50 approximation in the TUI).
    pub fn avg_latency_ms(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        (self.total_latency_us as f64 / self.total_requests as f64) / 1000.0
    }

    /// p50 approximation: returns average latency.
    pub fn p50_ms(&self) -> f64 {
        self.avg_latency_ms()
    }

    /// p95 approximation: returns average latency (best possible without per-sample storage).
    pub fn p95_ms(&self) -> f64 {
        self.avg_latency_ms()
    }
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

    pub async fn record_request(
        &self,
        route_key: &str,
        cache_hit: bool,
        latency_ms: f64,
        healthy: bool,
    ) {
        let latency_us = (latency_ms * 1000.0) as u64;

        // Fast path: read lock — no write contention on the hot path.
        {
            let stats = self.route_stats.read().await;
            if let Some(entry) = stats.get(route_key) {
                entry.total_requests.fetch_add(1, Ordering::Relaxed);
                if cache_hit {
                    entry.cache_hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    entry.cache_misses.fetch_add(1, Ordering::Relaxed);
                }
                if !healthy {
                    entry.error_count.fetch_add(1, Ordering::Relaxed);
                }
                entry.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);
                entry.healthy.store(healthy, Ordering::Relaxed);
                return;
            }
        }

        // Slow path: write lock only on first request to this route.
        let mut stats = self.route_stats.write().await;
        let entry = stats
            .entry(route_key.to_string())
            .or_insert_with(|| Arc::new(RouteStats::default()));
        entry.total_requests.fetch_add(1, Ordering::Relaxed);
        if cache_hit {
            entry.cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.cache_misses.fetch_add(1, Ordering::Relaxed);
        }
        if !healthy {
            entry.error_count.fetch_add(1, Ordering::Relaxed);
        }
        entry.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        entry.healthy.store(healthy, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn route_stats_fetch_add_is_atomic() {
        let counter = AtomicU64::new(0);
        counter.fetch_add(1, Ordering::Relaxed);
        counter.fetch_add(1, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn snapshot_avg_latency_ms_correct() {
        let s = RouteStats::default();
        s.total_requests.store(2, Ordering::Relaxed);
        // 10 ms + 30 ms = 40 000 us total
        s.total_latency_us.store(40_000, Ordering::Relaxed);
        let snap = s.snapshot();
        assert_eq!(snap.p50_ms(), 20.0);
    }

    #[test]
    fn snapshot_zero_requests_returns_zero_latency() {
        let s = RouteStats::default();
        let snap = s.snapshot();
        assert_eq!(snap.p50_ms(), 0.0);
        assert_eq!(snap.p95_ms(), 0.0);
    }

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
        Self { samples: VecDeque::new() }
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
            let idx = ((p * n as f64).ceil() as usize).saturating_sub(1).min(n - 1);
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
    fn default() -> Self { Self::new() }
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
    fn identical_samples_return_that_value() {
        let mut w = LatencyWindow::new();
        let now = Instant::now();
        for _ in 0..100 { w.push(now, 7.5); }
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
        for i in 1..=100 { w.push(now, i as f64); }
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
        for i in 1..=100 { w.push(now, i as f64); }
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
        for _ in 0..5 { w.push(old, 1.0); }
        let now = old + Duration::from_secs(301);
        for _ in 0..3 { w.push(now, 2.0); }
        assert_eq!(w.count(now), 3);
    }
}
