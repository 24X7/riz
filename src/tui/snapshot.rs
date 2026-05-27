use crate::process::{HostStats, PoolStats};
use crate::state::{AppState, FunctionStateSnapshot, LogEntry};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// Plain-data snapshot of everything the TUI needs for one render tick.
/// Populated by the async snapshotter task; consumed by the sync TUI thread
/// via `watch::Receiver::borrow()` — no RwLock contention on the hot path.
#[derive(Clone, Debug, Default)]
pub struct TuiSnapshot {
    pub functions: Vec<FunctionStateSnapshot>,
    pub pool_stats: Vec<PoolStats>,
    pub host_stats: HostStats,
    pub uptime_secs: u64,
    pub cache_entry_count: u64,
    /// Accumulated log entries (capped at 500 most-recent).
    pub log_entries: VecDeque<LogEntry>,
    /// Unix seconds when this snapshot was captured.
    pub captured_at_secs: u64,
}

/// Spawn the async snapshotter task on the given tokio runtime handle.
/// Runs at ~100 ms cadence; writes a fresh `TuiSnapshot` to the watch channel
/// on every tick.
///
/// Returns the `watch::Receiver` that the TUI thread reads from.
pub fn spawn_snapshotter(
    state: Arc<AppState>,
    handle: &tokio::runtime::Handle,
) -> watch::Receiver<TuiSnapshot> {
    let (tx, rx) = watch::channel(TuiSnapshot::default());
    handle.spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
        // Accumulated log buffer lives entirely in the snapshotter task.
        let mut log_buf: VecDeque<LogEntry> = VecDeque::new();

        loop {
            interval.tick().await;

            // Drain new log entries (try_lock is non-blocking).
            if let Ok(mut rx_guard) = state.log_rx.try_lock() {
                while let Ok(entry) = rx_guard.try_recv() {
                    log_buf.push_back(entry);
                    if log_buf.len() > 500 {
                        log_buf.pop_front();
                    }
                }
            }

            let now = Instant::now();
            let functions = {
                let guard = state.riz_state.functions.read().await;
                guard.values().map(|f| f.snapshot(now)).collect()
            };
            let pool_stats = state.process_manager.pool_stats().await;
            let host_stats = state.process_manager.host_stats();
            let uptime_secs = state.riz_state.uptime_secs();
            let cache_entry_count = state.cache.entry_count();
            let captured_at_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let snapshot = TuiSnapshot {
                functions,
                pool_stats,
                host_stats,
                uptime_secs,
                cache_entry_count,
                log_entries: log_buf.clone(),
                captured_at_secs,
            };

            // send returns Err only if all receivers have been dropped (TUI exited)
            if tx.send(snapshot).is_err() {
                break;
            }
        }
    });
    rx
}
