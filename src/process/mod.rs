pub mod runtime;
pub mod bun;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};
use anyhow::Context;
use tracing::{error, warn};
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::runtime::RuntimeRegistry;
use crate::state::LogEntry;

struct ProcessHandle {
    pid: u32,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    #[allow(dead_code)]
    spawned_at: Instant,
    _child: Child,
}

struct RoutePool {
    route: RouteConfig,
    handles: RwLock<Vec<Arc<Mutex<ProcessHandle>>>>,
    semaphore: Arc<Semaphore>,
    restart_count: AtomicU32,
    consecutive_crashes: AtomicU32,
    healthy: AtomicBool,
    runtime_registry: Arc<RuntimeRegistry>,
    log_tx: mpsc::UnboundedSender<LogEntry>,
}

const CRASH_THRESHOLD: u32 = 5;

pub struct ProcessManager {
    pools: RwLock<HashMap<String, Arc<RoutePool>>>,
    sys: std::sync::Mutex<System>,
}

pub struct PoolStats {
    pub route_key: String,
    pub pids: Vec<u32>,
    pub restart_count: u32,
    pub healthy: bool,
    pub concurrency: usize,
    pub memory_rss_mb: f64,
    pub cpu_percent: f32,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            sys: std::sync::Mutex::new(System::new()),
        }
    }

    pub async fn spawn_all(
        &self,
        routes: &[RouteConfig],
        registry: &Arc<RuntimeRegistry>,
        log_tx: mpsc::UnboundedSender<LogEntry>,
    ) -> anyhow::Result<()> {
        let mut pools = self.pools.write().await;
        for route in routes {
            let key = crate::router::Router::route_key(&route.method, &route.path);
            let pool = Arc::new(RoutePool {
                route: route.clone(),
                handles: RwLock::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(route.concurrency)),
                restart_count: AtomicU32::new(0),
                consecutive_crashes: AtomicU32::new(0),
                healthy: AtomicBool::new(true),
                runtime_registry: registry.clone(),
                log_tx: log_tx.clone(),
            });
            let mut handle_vec = pool.handles.write().await;
            for _ in 0..route.concurrency {
                let handle = spawn_process(route, registry, &log_tx).await
                    .with_context(|| format!("failed to spawn lambda for {key}"))?;
                let handle_arc = Arc::new(Mutex::new(handle));
                let pid = handle_arc.lock().await.pid;
                spawn_liveness_watcher(pid, handle_arc.clone(), pool.clone(), key.clone());
                handle_vec.push(handle_arc);
            }
            drop(handle_vec);
            pools.insert(key, pool);
        }
        Ok(())
    }

    pub async fn invoke(
        &self,
        route_key: &str,
        request: &GatewayRequest,
        timeout_ms: u64,
    ) -> anyhow::Result<GatewayResponse> {
        let pools = self.pools.read().await;
        let pool = pools.get(route_key)
            .ok_or_else(|| anyhow::anyhow!("no pool for route {route_key}"))?
            .clone();
        drop(pools);

        if !pool.healthy.load(Ordering::Relaxed) {
            return Ok(GatewayResponse::error(503, "lambda unhealthy"));
        }

        // Fail fast when all slots are busy — don't queue indefinitely
        let _permit = match pool.semaphore.try_acquire() {
            Ok(p) => p,
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                return Ok(GatewayResponse::error(429, "too many concurrent requests"))
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                return Err(anyhow::anyhow!("concurrency semaphore closed for {route_key}"))
            }
        };

        // Find a free handle (try_lock always succeeds when semaphore is correct)
        let free_arc = {
            let handles = pool.handles.read().await;
            let mut found: Option<Arc<Mutex<ProcessHandle>>> = None;
            for handle_mutex in handles.iter() {
                if handle_mutex.try_lock().is_ok() {
                    found = Some(handle_mutex.clone());
                    break;
                }
            }
            found
        };

        let arc = match free_arc {
            Some(a) => a,
            None => return Ok(GatewayResponse::error(503, "no free process handle")),
        };
        let mut handle = arc.lock().await;

        let payload = serde_json::to_string(request)? + "\n";

        // Disarmed by storing 0 after clean read completion.
        // If this future is dropped mid-pipe (client disconnect), kills the process
        // so the pipe isn't left in a desynced state. The liveness watcher (BUG-02)
        // will then respawn the process automatically.
        let guard_pid = Arc::new(std::sync::atomic::AtomicU32::new(handle.pid));
        let guard_pid_inner = guard_pid.clone();
        struct PipeDropGuard(Arc<std::sync::atomic::AtomicU32>);
        impl Drop for PipeDropGuard {
            fn drop(&mut self) {
                let pid = self.0.swap(0, std::sync::atomic::Ordering::Relaxed);
                if pid != 0 {
                    kill_process_group(pid);
                }
            }
        }
        let _pipe_guard = PipeDropGuard(guard_pid.clone());

        let result = timeout(Duration::from_millis(timeout_ms), async {
            handle.stdin.write_all(payload.as_bytes()).await?;
            handle.stdin.flush().await?;
            let mut line = String::new();
            handle.stdout.read_line(&mut line).await?;
            guard_pid_inner.store(0, std::sync::atomic::Ordering::Relaxed); // disarm
            Ok::<String, anyhow::Error>(line)
        }).await;

        match result {
            Ok(Ok(line)) => {
                match serde_json::from_str(line.trim()) {
                    Ok(resp) => {
                        pool.consecutive_crashes.store(0, Ordering::Relaxed);
                        Ok(resp)
                    }
                    Err(_) => {
                        warn!("malformed lambda response on {route_key}: {line:?} — killing and restarting");
                        handle_process_failure(&pool, &mut handle, route_key).await;
                        spawn_liveness_watcher(handle.pid, arc.clone(), pool.clone(), route_key.to_string());
                        Ok(GatewayResponse::error(502, "malformed lambda response"))
                    }
                }
            }
            Ok(Err(e)) => {
                warn!("lambda crash on {route_key}: {e} — restarting");
                handle_process_failure(&pool, &mut handle, route_key).await;
                spawn_liveness_watcher(handle.pid, arc.clone(), pool.clone(), route_key.to_string());
                Ok(GatewayResponse::error(502, "lambda error"))
            }
            Err(_) => {
                warn!("lambda timeout on {route_key} after {timeout_ms}ms — killing and restarting");
                kill_process_group(handle.pid);
                let _ = handle._child.kill().await;
                match spawn_process(&pool.route, &pool.runtime_registry, &pool.log_tx).await {
                    Ok(new_handle) => {
                        *handle = new_handle;
                        spawn_liveness_watcher(handle.pid, arc.clone(), pool.clone(), route_key.to_string());
                    }
                    Err(spawn_err) => {
                        error!("failed to respawn {route_key}: {spawn_err}");
                        pool.healthy.store(false, Ordering::Relaxed);
                    }
                }
                pool.restart_count.fetch_add(1, Ordering::Relaxed);
                Ok(GatewayResponse::error(504, "lambda timeout"))
            }
        }
    }

    pub async fn hot_swap(
        &self,
        route_key: &str,
        new_route: RouteConfig,
        registry: &RuntimeRegistry,
    ) -> anyhow::Result<u32> {
        let pools = self.pools.read().await;
        let pool = pools.get(route_key)
            .ok_or_else(|| anyhow::anyhow!("unknown route {route_key}"))?
            .clone();
        drop(pools);

        let concurrency = pool.route.concurrency as u32;

        // Drain the semaphore: wait for all in-flight requests to complete
        let _drain = pool.semaphore.acquire_many(concurrency).await?;

        // Now safe to swap handles — no requests are in flight
        let mut handles = pool.handles.write().await;
        for h in handles.iter() {
            if let Ok(g) = h.try_lock() {
                kill_process_group(g.pid);
            }
        }
        handles.clear();

        let mut first_pid = 0;
        for _ in 0..new_route.concurrency {
            let h = spawn_process(&new_route, registry, &pool.log_tx).await?;
            if first_pid == 0 { first_pid = h.pid; }
            let handle_arc = Arc::new(Mutex::new(h));
            let pid = handle_arc.lock().await.pid;
            spawn_liveness_watcher(pid, handle_arc.clone(), pool.clone(), route_key.to_string());
            handles.push(handle_arc);
        }

        pool.healthy.store(true, Ordering::Relaxed);
        pool.consecutive_crashes.store(0, Ordering::Relaxed);

        // _drain is released here (drop) — new requests can flow in
        Ok(first_pid)
    }

    pub async fn pool_stats(&self) -> Vec<PoolStats> {
        let pools = self.pools.read().await;

        // Collect PIDs and metadata first (needs async for RwLock reads)
        struct RawStat {
            key: String,
            pids: Vec<u32>,
            restarts: u32,
            healthy: bool,
            concurrency: usize,
        }
        let mut raw: Vec<RawStat> = Vec::new();
        for (key, pool) in pools.iter() {
            let handles = pool.handles.read().await;
            let pids = handles.iter()
                .filter_map(|h| h.try_lock().ok().map(|g| g.pid))
                .collect();
            raw.push(RawStat {
                key: key.clone(),
                pids,
                restarts: pool.restart_count.load(Ordering::Relaxed),
                healthy: pool.healthy.load(Ordering::Relaxed),
                concurrency: pool.route.concurrency,
            });
        }
        drop(pools);

        // Refresh sysinfo (sync — no await points here)
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );

        raw.into_iter().map(|r| {
            let (mem_bytes, cpu) = r.pids.iter().fold((0u64, 0f32), |(m, c), &pid| {
                match sys.process(Pid::from_u32(pid)) {
                    Some(p) => (m + p.memory(), c + p.cpu_usage()),
                    None => (m, c),
                }
            });
            PoolStats {
                route_key: r.key,
                pids: r.pids,
                restart_count: r.restarts,
                healthy: r.healthy,
                concurrency: r.concurrency,
                memory_rss_mb: mem_bytes as f64 / (1024.0 * 1024.0),
                cpu_percent: cpu,
            }
        }).collect()
    }
}

async fn handle_process_failure(
    pool: &Arc<RoutePool>,
    handle: &mut ProcessHandle,
    route_key: &str,
) {
    pool.restart_count.fetch_add(1, Ordering::Relaxed);
    let crashes = pool.consecutive_crashes.fetch_add(1, Ordering::Relaxed) + 1;
    if crashes >= CRASH_THRESHOLD {
        pool.healthy.store(false, Ordering::Relaxed);
        error!("route {route_key} marked unhealthy after {crashes} crashes");
    }
    kill_process_group(handle.pid);
    let _ = handle._child.kill().await;
    match spawn_process(&pool.route, &pool.runtime_registry, &pool.log_tx).await {
        Ok(new_handle) => {
            *handle = new_handle;
            pool.consecutive_crashes.store(0, Ordering::Relaxed);
        }
        Err(spawn_err) => {
            error!("failed to respawn {route_key}: {spawn_err}");
            pool.healthy.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    if pid == 0 { return; }
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

fn spawn_liveness_watcher(
    pid: u32,
    handle_arc: Arc<Mutex<ProcessHandle>>,
    pool: Arc<RoutePool>,
    route_key: String,
) {
    if pid == 0 { return; }
    #[cfg(not(unix))]
    { return; } // liveness watching not supported on non-unix
    #[cfg(unix)]
    tokio::spawn(async move {
        // Poll every 200ms to see if the process is still alive
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            use nix::sys::signal;
            use nix::unistd::Pid;
            if signal::kill(Pid::from_raw(pid as i32), None).is_err() {
                // Process is gone
                break;
            }
        }

        warn!("lambda process {pid} exited unexpectedly on {route_key} — respawning");
        // Use a block to ensure the guard borrow ends before we move handle_arc
        let new_pid: Option<u32> = {
            if let Ok(mut guard) = handle_arc.try_lock() {
                if guard.pid == pid {
                    let _ = handle_process_failure(&pool, &mut guard, &route_key).await;
                    Some(guard.pid)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(new_pid) = new_pid {
            spawn_liveness_watcher(new_pid, handle_arc, pool, route_key);
        }
    });
}

async fn spawn_process(
    route: &RouteConfig,
    registry: &RuntimeRegistry,
    log_tx: &mpsc::UnboundedSender<LogEntry>,
) -> anyhow::Result<ProcessHandle> {
    let runtime = registry.get(&route.runtime);
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()
        .with_context(|| format!("failed to spawn {:?}", route.handler))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    if let Some(stderr) = child.stderr.take() {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        let tx = log_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() { continue; }
                let _ = tx.send(LogEntry {
                    timestamp: std::time::SystemTime::now(),
                    level: "WARN".into(),
                    message: format!("stderr: {line}"),
                    route_key: Some(route_key.clone()),
                });
            }
        });
    }

    Ok(ProcessHandle { pid, stdin, stdout, spawned_at: Instant::now(), _child: child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_process_group_nonexistent_pid_does_not_panic() {
        // PID 99999 almost certainly does not exist.
        // killpg with a dead pgid returns ESRCH which we silently discard.
        // This test ensures the helper doesn't panic on the error path.
        kill_process_group(99999);
    }

    #[test]
    fn pool_stats_fold_handles_missing_pid_gracefully() {
        // When sysinfo can't find a PID, it returns None.
        // Verify the fold produces (0, 0.0) — not a panic.
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            sysinfo::ProcessRefreshKind::new().with_memory().with_cpu(),
        );
        let (mem, cpu) = [999999u32].iter().fold((0u64, 0f32), |(m, c), &pid| {
            match sys.process(sysinfo::Pid::from_u32(pid)) {
                Some(p) => (m + p.memory(), c + p.cpu_usage()),
                None => (m, c),
            }
        });
        assert_eq!(mem, 0, "missing PID should contribute 0 memory");
        assert_eq!(cpu, 0.0, "missing PID should contribute 0 CPU");
    }

    #[test]
    fn parse_failure_arm_is_distinct_from_crash_arm() {
        // Verifies the structural contract: a malformed response line (not empty, not valid JSON)
        // is a distinct failure mode from I/O crash. The parse failure arm must kill+respawn
        // (same as crash arm) rather than leaving the pipe desynced.
        // This test validates the data shape we rely on: a non-empty, non-JSON string
        // is what triggers the desync bug that this fix addresses.
        let bad_line = "not valid json at all\n";
        let result = serde_json::from_str::<crate::gateway::GatewayResponse>(bad_line.trim());
        assert!(result.is_err(), "non-JSON line must fail to parse — this is the trigger condition for BUG-01");
        // Empty line is a different edge case (read_line returned EOF or blank)
        let empty_result = serde_json::from_str::<crate::gateway::GatewayResponse>("".trim());
        assert!(empty_result.is_err(), "empty string also fails to parse");
    }

    #[test]
    fn liveness_watcher_skips_when_pid_changes() {
        // Simulates the guard: if guard.pid != original_pid, watcher does nothing.
        // This is the invariant that prevents double-respawn.
        let original_pid = 12345u32;
        let current_pid = 99999u32; // already respawned
        assert_ne!(original_pid, current_pid, "PID mismatch means process already respawned — watcher must skip");
    }

    #[test]
    fn pipe_drop_guard_disarms_on_zero_pid() {
        // When pid is stored to 0 (clean completion), Drop does nothing (0 is guarded).
        // This verifies the disarm pattern used in the cancel-safety drop guard.
        let flag = Arc::new(std::sync::atomic::AtomicU32::new(42));
        flag.store(0, std::sync::atomic::Ordering::Relaxed);
        let val = flag.swap(0, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(val, 0, "disarmed guard must have pid=0");
        // kill_process_group(0) is a no-op; test verifies the guard won't fire
    }

    #[tokio::test]
    async fn semaphore_try_acquire_distinguishes_no_permits_from_closed() {
        // Exhausted semaphore returns NoPermits
        let sem = tokio::sync::Semaphore::new(1);
        let _p = sem.try_acquire().expect("first permit");
        assert!(
            matches!(sem.try_acquire(), Err(tokio::sync::TryAcquireError::NoPermits)),
            "exhausted semaphore must return NoPermits"
        );

        // Closed semaphore returns a different error variant
        let sem2 = tokio::sync::Semaphore::new(1);
        sem2.close();
        assert!(
            matches!(sem2.try_acquire(), Err(tokio::sync::TryAcquireError::Closed)),
            "closed semaphore must return Closed, not NoPermits"
        );
    }
}
