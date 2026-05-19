pub mod runtime;
pub mod bun;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};
use anyhow::Context;
use tracing::{error, warn};
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::runtime::RuntimeRegistry;

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
}

const CRASH_THRESHOLD: u32 = 5;

pub struct ProcessManager {
    pools: RwLock<HashMap<String, Arc<RoutePool>>>,
}

pub struct PoolStats {
    pub route_key: String,
    pub pids: Vec<u32>,
    pub restart_count: u32,
    pub healthy: bool,
    pub concurrency: usize,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self { pools: RwLock::new(HashMap::new()) }
    }

    pub async fn spawn_all(
        &self,
        routes: &[RouteConfig],
        registry: &RuntimeRegistry,
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
            });
            let mut handle_vec = pool.handles.write().await;
            for _ in 0..route.concurrency {
                let handle = spawn_process(route, registry).await
                    .with_context(|| format!("failed to spawn lambda for {key}"))?;
                handle_vec.push(Arc::new(Mutex::new(handle)));
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

        // Acquire permit: guarantees at least one handle is free
        let _permit = pool.semaphore.acquire().await?;

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
        let result = timeout(Duration::from_millis(timeout_ms), async {
            handle.stdin.write_all(payload.as_bytes()).await?;
            handle.stdin.flush().await?;
            let mut line = String::new();
            handle.stdout.read_line(&mut line).await?;
            Ok::<String, anyhow::Error>(line)
        }).await;

        match result {
            Ok(Ok(line)) => {
                pool.consecutive_crashes.store(0, Ordering::Relaxed);
                serde_json::from_str(line.trim())
                    .map_err(|_| anyhow::anyhow!("malformed lambda response: {line}"))
            }
            Ok(Err(e)) => {
                pool.restart_count.fetch_add(1, Ordering::Relaxed);
                let crashes = pool.consecutive_crashes.fetch_add(1, Ordering::Relaxed) + 1;
                if crashes >= CRASH_THRESHOLD {
                    pool.healthy.store(false, Ordering::Relaxed);
                    error!("route {route_key} marked unhealthy after {crashes} crashes");
                }
                warn!("lambda crash on {route_key}: {e}");
                Ok(GatewayResponse::error(502, "lambda error"))
            }
            Err(_) => {
                warn!("lambda timeout on {route_key} after {timeout_ms}ms");
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
        handles.clear();

        let mut first_pid = 0;
        for _ in 0..new_route.concurrency {
            let h = spawn_process(&new_route, registry).await?;
            if first_pid == 0 { first_pid = h.pid; }
            handles.push(Arc::new(Mutex::new(h)));
        }

        pool.healthy.store(true, Ordering::Relaxed);
        pool.consecutive_crashes.store(0, Ordering::Relaxed);

        // _drain is released here (drop) — new requests can flow in
        Ok(first_pid)
    }

    pub async fn pool_stats(&self) -> Vec<PoolStats> {
        let pools = self.pools.read().await;
        let mut stats = Vec::new();
        for (key, pool) in pools.iter() {
            let handles = pool.handles.read().await;
            stats.push(PoolStats {
                route_key: key.clone(),
                pids: handles.iter()
                    .filter_map(|h| h.try_lock().ok().map(|g| g.pid))
                    .collect(),
                restart_count: pool.restart_count.load(Ordering::Relaxed),
                healthy: pool.healthy.load(Ordering::Relaxed),
                concurrency: pool.route.concurrency,
            });
        }
        stats
    }
}

async fn spawn_process(
    route: &RouteConfig,
    registry: &RuntimeRegistry,
) -> anyhow::Result<ProcessHandle> {
    use tokio::io::AsyncReadExt;

    let runtime = registry.get(&route.runtime);
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .with_context(|| format!("failed to spawn {:?}", route.handler))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    if let Some(mut stderr) = child.stderr.take() {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf).await;
            if !buf.trim().is_empty() {
                warn!("lambda stderr [{}]: {}", route_key, buf.trim());
            }
        });
    }

    Ok(ProcessHandle { pid, stdin, stdout, spawned_at: Instant::now(), _child: child })
}
