use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{Mutex, RwLock};
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
    pub route_stats: RwLock<HashMap<String, RouteStats>>,
    pub log_buffer: Mutex<VecDeque<LogEntry>>,
}

#[derive(Default, Clone)]
pub struct RouteStats {
    pub request_count: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub latencies_ms: VecDeque<f64>,
    pub healthy: bool,
}

impl RouteStats {
    pub fn p50_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.5)
    }

    pub fn p95_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.95)
    }
}

fn percentile(values: &VecDeque<f64>, p: f64) -> f64 {
    if values.is_empty() { return 0.0; }
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((sorted.len() as f64) * p).min((sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub level: String,
    pub message: String,
    pub route_key: Option<String>,
}

impl AppState {
    pub async fn push_log(&self, level: &str, route_key: Option<&str>, message: String) {
        let mut buf = self.log_buffer.lock().await;
        if buf.len() >= 200 {
            buf.pop_front();
        }
        buf.push_back(LogEntry {
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
        let mut stats = self.route_stats.write().await;
        let entry = stats.entry(route_key.to_string()).or_default();
        entry.request_count += 1;
        entry.healthy = healthy;
        if cache_hit { entry.cache_hits += 1; } else { entry.cache_misses += 1; }
        entry.latencies_ms.push_back(latency_ms);
        if entry.latencies_ms.len() > 100 {
            entry.latencies_ms.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stats(latencies: &[f64]) -> RouteStats {
        let mut s = RouteStats::default();
        for &v in latencies {
            s.latencies_ms.push_back(v);
        }
        s
    }

    #[test]
    fn p50_of_sorted_values() {
        let s = make_stats(&[10.0, 20.0, 30.0, 40.0, 50.0]);
        assert_eq!(s.p50_ms(), 30.0);
    }

    #[test]
    fn p95_of_sorted_values() {
        // 5 values, p95 index = floor(5 * 0.95) = 4 (last element)
        let s = make_stats(&[10.0, 20.0, 30.0, 40.0, 100.0]);
        assert_eq!(s.p95_ms(), 100.0);
    }

    #[test]
    fn empty_returns_zero() {
        let s = RouteStats::default();
        assert_eq!(s.p50_ms(), 0.0);
        assert_eq!(s.p95_ms(), 0.0);
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
