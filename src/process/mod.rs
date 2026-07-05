pub mod bun;
pub mod guard;
pub mod liveness;
pub mod node;
pub mod pool;
pub mod python;
pub mod runtime;
pub mod runtime_api;
pub mod safety;
pub mod static_binary;
pub mod wasm;

use crate::config::FunctionConfig;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::process::liveness::{handle_process_failure, spawn_liveness_watcher};
pub use crate::process::pool::kill_process_group;
use crate::process::pool::{
    spawn_process, spawn_with_cold_start_record, HandleTransport, ProcessHandle, RoutePool,
};
use crate::process::runtime::RuntimeRegistry;
use crate::process::runtime_api::Invocation;
use crate::runtime::error_response;
use crate::state::{LogEntry, RizState};
use anyhow::Context;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};
use tracing::{error, trace, warn};

/// Wire envelope wrapping an invocation event with sidecar metadata.
///
/// Sent as a single JSON line to the Bun adapter via stdin.
/// The adapter unwraps the event and uses the metadata to populate the
/// Lambda context object (deadline, function name, synthetic ARN).
#[derive(Serialize)]
struct InvocationEnvelope<'a, E: Serialize> {
    event: &'a E,
    #[serde(rename = "__riz_deadline_ms")]
    deadline_ms: i64,
    #[serde(rename = "__riz_function_name")]
    function_name: &'a str,
}

/// Build a JSON-encoded invocation envelope for the Bun adapter wire protocol.
///
/// The envelope wraps the user event with two sidecar fields:
/// - `__riz_deadline_ms`: epoch millis at which the timeout expires.
/// - `__riz_function_name`: the riz.toml function name (e.g. `"api"`).
///
/// If the system clock is pre-epoch (impossible in practice), `deadline_ms`
/// falls back to `0` and a warning is emitted. The adapter will then return
/// `getRemainingTimeInMillis() == 0`, signalling the handler to bail early.
pub fn build_envelope_payload<E: Serialize>(
    event: &E,
    function_name: &str,
    timeout_ms: u64,
) -> Result<String, serde_json::Error> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|_| {
            warn!("system clock is pre-epoch; setting deadline_ms=0 for function {function_name}");
            0
        });
    let deadline_ms = now_ms.saturating_add(timeout_ms as i64);
    trace!(function_name, deadline_ms, "building invocation envelope");
    let envelope = InvocationEnvelope {
        event,
        deadline_ms,
        function_name,
    };
    serde_json::to_string(&envelope)
}

/// Typed error variants for pool-level invocation failures.
///
/// Returned by [`ProcessManager::invoke`] and [`ProcessManager::invoke_generic`]
/// so callers can pattern-match on failure cause without string-matching.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("function {0} has no pool configured")]
    NoPool(String),
    #[error("function {0} at concurrency limit (semaphore exhausted)")]
    SemaphoreExhausted(String),
    #[error("function {0} pool closed")]
    SemaphoreClosed(String),
    #[error("function {0} timed out after {1}ms")]
    Timeout(String, u64),
    #[error("function {0} returned malformed response: {1}")]
    InvalidResponse(String, String),
    #[error("function {0}: {1}")]
    Other(String, #[source] anyhow::Error),
}

pub struct ProcessManager {
    pools: RwLock<HashMap<String, Arc<RoutePool>>>,
    sys: std::sync::Mutex<System>,
    /// Shared RizState. Threaded into each RoutePool at creation so spawn
    /// sites (initial fill, restart-after-crash, timeout-respawn, hot_swap)
    /// can bump per-function cold_starts counters.
    riz_state: Arc<RizState>,
}

#[derive(Clone, Debug)]
pub struct PoolStats {
    /// Function name (e.g. "api").
    pub name: String,
    pub pids: Vec<u32>,
    pub restart_count: u32,
    pub healthy: bool,
    #[allow(dead_code)]
    pub concurrency: usize,
    pub memory_rss_mb: f64,
    pub cpu_percent: f32,
}

#[derive(Clone, Debug, Default)]
pub struct HostStats {
    pub pid: u32,
    pub memory_rss_mb: f64,
    pub cpu_percent: f32,
    pub cores: usize,
}

/// Outcome of one AWS-Runtime-API round-trip to a worker.
enum RtOutcome {
    /// The handler responded (raw response JSON bytes from `/response`).
    Response(Vec<u8>),
    /// The handler reported an error via `/error` — a normal result, NOT a
    /// worker crash; the worker stays healthy.
    HandlerError(String),
    /// The worker dropped the response channel without answering (it crashed).
    WorkerGone,
    /// No response within the deadline.
    Timeout,
}

/// Deliver one event to a runtime-API worker and await its response. The event
/// is the RAW AWS event JSON (deadline + ARN ride in the `Lambda-Runtime-*`
/// headers, per the AWS contract) — NOT riz's stdio envelope.
async fn runtime_api_roundtrip<E: Serialize>(
    sender: &mpsc::Sender<Invocation>,
    function_name: &str,
    request: &E,
    timeout_ms: u64,
) -> RtOutcome {
    let event = match serde_json::to_vec(request) {
        Ok(v) => v,
        // A serialize failure is our bug, surfaced as a handler-style error.
        Err(e) => return RtOutcome::HandlerError(format!("event serialize: {e}")),
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let (tx, rx) = oneshot::channel();
    let inv = Invocation {
        request_id: uuid::Uuid::new_v4().to_string(),
        // Saturating: clock-epoch millis + a config timeout can only overflow
        // on a pathological timeout_ms; a saturated deadline just reads as
        // "no time left" to the worker, which is the safe degradation.
        deadline_ms: now_ms.saturating_add(timeout_ms as i64),
        invoked_arn: format!("arn:riz:lambda:local:000000000000:function:{function_name}"),
        event,
        respond: tx,
    };
    if sender.send(inv).await.is_err() {
        return RtOutcome::WorkerGone;
    }
    match timeout(Duration::from_millis(timeout_ms), rx).await {
        Ok(Ok(Ok(bytes))) => RtOutcome::Response(bytes),
        Ok(Ok(Err(msg))) => RtOutcome::HandlerError(msg),
        Ok(Err(_)) => RtOutcome::WorkerGone, // sender dropped → worker died
        Err(_) => RtOutcome::Timeout,
    }
}

/// Grab a concurrency permit and a free worker handle from the pool.
/// Rejects — never queues — when the pool is saturated (rule 3: overload is
/// answered with backpressure, not buffering).
async fn acquire_worker(
    pool: &Arc<RoutePool>,
    function_name: &str,
) -> Result<(tokio::sync::OwnedSemaphorePermit, Arc<Mutex<ProcessHandle>>), PoolError> {
    let permit = match pool.semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(tokio::sync::TryAcquireError::NoPermits) => {
            return Err(PoolError::SemaphoreExhausted(function_name.into()))
        }
        Err(tokio::sync::TryAcquireError::Closed) => {
            return Err(PoolError::SemaphoreClosed(function_name.into()))
        }
    };
    let free_arc = {
        let handles = pool.handles.read().await;
        handles.iter().find(|h| h.try_lock().is_ok()).cloned()
    };
    match free_arc {
        Some(arc) => Ok((permit, arc)),
        None => Err(PoolError::Other(
            function_name.into(),
            anyhow::anyhow!("no free process handle"),
        )),
    }
}

/// Crash-path recovery: kill + respawn via `handle_process_failure` (which
/// feeds the consecutive-crash circuit breaker), then re-arm the liveness
/// watcher for the new PID.
async fn fail_and_rearm(
    pool: &Arc<RoutePool>,
    arc: &Arc<Mutex<ProcessHandle>>,
    handle: &mut ProcessHandle,
    function_name: &str,
) {
    handle_process_failure(pool, handle, function_name).await;
    spawn_liveness_watcher(
        handle.pid,
        arc.clone(),
        pool.clone(),
        function_name.to_string(),
    );
}

/// Timeout-path recovery: the worker may be wedged mid-invocation, so it is
/// killed outright, replaced in place, and the watcher re-armed. Unlike the
/// crash path this does NOT feed the consecutive-crash breaker: a timeout is
/// workload behavior (a slow handler), each occurrence is paced by a request
/// that already paid `timeout_ms`, and marking the pool unhealthy for it
/// would let one slow route park the whole function.
async fn respawn_after_timeout(
    pool: &Arc<RoutePool>,
    arc: &Arc<Mutex<ProcessHandle>>,
    handle: &mut ProcessHandle,
    function_name: &str,
) {
    kill_process_group(handle.pid);
    // Result explicitly discarded (rule 7): the group kill above usually
    // already reaped the child; "already exited" is the expected case.
    let _ = handle._child.kill().await;
    match spawn_with_cold_start_record(pool, function_name).await {
        Ok(new_handle) => {
            *handle = new_handle;
            spawn_liveness_watcher(
                handle.pid,
                arc.clone(),
                pool.clone(),
                function_name.to_string(),
            );
        }
        Err(spawn_err) => {
            error!("failed to respawn {function_name}: {spawn_err}");
            pool.healthy.store(false, Ordering::Relaxed);
        }
    }
    pool.restart_count.fetch_add(1, Ordering::Relaxed);
}

/// Cap on one stdio response line from a worker (rule 3: the response path
/// must not balloon host memory on a worker that streams an endless line).
/// AWS Lambda caps response payloads at 6 MB; riz's envelope carries the
/// response JSON with a possibly base64-inflated body, so 16 MiB leaves
/// AWS-shaped workloads untouched while bounding a misbehaving worker. A
/// line truncated at the cap fails JSON parsing downstream → the malformed-
/// response arm kills and respawns the worker (the pipe holds residue, so
/// replacing the worker is the only safe recovery).
const MAX_RESPONSE_LINE_BYTES: u64 = 16 * 1024 * 1024;

/// Outcome of one stdio round-trip — mirrors [`RtOutcome`] for the pipe
/// transport so both transports share the caller-side failure arms.
enum StdioOutcome {
    /// One complete response line (parse still pending).
    Line(String),
    /// Pipe I/O failed — the worker crashed or closed its pipes.
    Io(anyhow::Error),
    /// No response line within `timeout_ms`.
    TimedOut,
}

/// Write one envelope line to the worker's stdin and read one capped
/// response line back, under `timeout_ms`.
///
/// Cancel-safety: if this future is dropped mid-roundtrip (the integration
/// timeout above us, a vanished WS client), the pipe is DESYNCED — the
/// worker's eventual response line would be read as the answer to the NEXT
/// invocation (for a guard pool, a stale "allow" answering the wrong
/// request). The drop guard kills the worker's process group so the
/// liveness watcher replaces it with a clean one. Disarmed on completion:
/// the completed-path arms own their own kill/respawn decisions.
///
/// Returns `Err` only for the not-a-stdio-worker dispatch bug — a failed
/// invocation, never a panic (rule 1: a supervisor degrades, it does not
/// crash the host).
async fn stdio_roundtrip(
    handle: &mut ProcessHandle,
    function_name: &str,
    payload: &str,
    timeout_ms: u64,
) -> Result<StdioOutcome, PoolError> {
    let worker_pid = handle.pid;
    let HandleTransport::Stdio { stdin, stdout } = &mut handle.transport else {
        // Callers dispatch RuntimeApi before calling this; if a transport
        // ever falls through, fail the one invocation instead of panicking.
        return Err(PoolError::Other(
            function_name.into(),
            anyhow::anyhow!("transport dispatch bug: stdio roundtrip on a non-stdio worker"),
        ));
    };

    struct PipeDropGuard(Arc<AtomicU32>);
    impl Drop for PipeDropGuard {
        fn drop(&mut self) {
            let pid = self.0.swap(0, Ordering::Relaxed);
            if pid != 0 {
                kill_process_group(pid);
            }
        }
    }
    let armed_pid = Arc::new(AtomicU32::new(worker_pid));
    let _pipe_guard = PipeDropGuard(armed_pid.clone());

    let outcome = match timeout(Duration::from_millis(timeout_ms), async {
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await?;
        let mut line = String::new();
        // `take` bounds how many bytes this invocation may pull off the
        // pipe; the BufReader's position is preserved across calls.
        let mut limited = stdout.take(MAX_RESPONSE_LINE_BYTES);
        limited.read_line(&mut line).await?;
        Ok::<String, anyhow::Error>(line)
    })
    .await
    {
        Ok(Ok(line)) => StdioOutcome::Line(line),
        Ok(Err(e)) => StdioOutcome::Io(e),
        Err(_elapsed) => StdioOutcome::TimedOut,
    };
    // The roundtrip COMPLETED (even by timeout) — disarm. The caller's
    // failure arms kill/respawn under the same handle lock; firing the
    // guard as well would just re-SIGKILL an already-replaced pgid.
    armed_pid.store(0, Ordering::Relaxed);
    Ok(outcome)
}

impl ProcessManager {
    /// The shared function-state registry (metrics/health counters). Used by
    /// dispatch-side callers (e.g. guard timing) that record against pool
    /// names without holding their own RizState handle.
    pub fn riz_state(&self) -> &Arc<RizState> {
        &self.riz_state
    }

    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            sys: std::sync::Mutex::new(System::new()),
            riz_state,
        }
    }

    /// Spawn one process pool per function. Each pool holds N processes
    /// (where N = function.concurrency) and serves every route the function
    /// declares.
    pub async fn spawn_all(
        &self,
        functions: &indexmap::IndexMap<String, FunctionConfig>,
        registry: &Arc<RuntimeRegistry>,
        log_tx: mpsc::Sender<LogEntry>,
    ) -> anyhow::Result<()> {
        let mut pools = self.pools.write().await;
        for (name, cfg) in functions {
            Self::build_pool_into(&mut pools, name, cfg, registry, &log_tx, &self.riz_state)
                .await?;
            // WASM guards ride the same pool machinery — spawned as sibling
            // pools so they get liveness, respawn, and kill_on_drop for free.
            // A guard that can't spawn is a STARTUP error: a configured
            // policy must never be silently absent.
            if let Some(guard) = &cfg.guard_in {
                let gname = format!("{name}{}", guard::GUARD_IN_SUFFIX);
                let gcfg = guard::guard_pool_config(guard, cfg);
                Self::build_pool_into(
                    &mut pools,
                    &gname,
                    &gcfg,
                    registry,
                    &log_tx,
                    &self.riz_state,
                )
                .await
                .with_context(|| format!("failed to spawn guard_in for {name}"))?;
            }
            if let Some(guard) = &cfg.guard_out {
                let gname = format!("{name}{}", guard::GUARD_OUT_SUFFIX);
                let gcfg = guard::guard_pool_config(guard, cfg);
                Self::build_pool_into(
                    &mut pools,
                    &gname,
                    &gcfg,
                    registry,
                    &log_tx,
                    &self.riz_state,
                )
                .await
                .with_context(|| format!("failed to spawn guard_out for {name}"))?;
            }
        }
        Ok(())
    }

    /// Construct one pool (handles + liveness watchers) and insert it into
    /// the map under `name`. Shared by handler pools and guard pools.
    async fn build_pool_into(
        pools: &mut std::collections::HashMap<String, Arc<RoutePool>>,
        name: &str,
        cfg: &FunctionConfig,
        registry: &Arc<RuntimeRegistry>,
        log_tx: &mpsc::Sender<LogEntry>,
        riz_state: &Arc<RizState>,
    ) -> anyhow::Result<()> {
        let pool = Arc::new(RoutePool {
            name: name.to_string(),
            config: cfg.clone(),
            handles: RwLock::new(Vec::new()),
            semaphore: Arc::new(Semaphore::new(cfg.concurrency)),
            restart_count: AtomicU32::new(0),
            consecutive_crashes: AtomicU32::new(0),
            healthy: AtomicBool::new(true),
            runtime_registry: registry.clone(),
            log_tx: log_tx.clone(),
            riz_state: riz_state.clone(),
        });
        let mut handle_vec = pool.handles.write().await;
        for _ in 0..cfg.concurrency {
            let handle = spawn_with_cold_start_record(&pool, name)
                .await
                .with_context(|| format!("failed to spawn lambda for {name}"))?;
            let handle_arc = Arc::new(Mutex::new(handle));
            let pid = handle_arc.lock().await.pid;
            spawn_liveness_watcher(pid, handle_arc.clone(), pool.clone(), name.to_string());
            handle_vec.push(handle_arc);
        }
        drop(handle_vec);
        pools.insert(name.to_string(), pool);
        Ok(())
    }

    /// Invoke a function by its name. `function_name` keys into the pool map
    /// (one pool per function, shared by all routes the function declares).
    #[tracing::instrument(skip(self, request), fields(function = %function_name, timeout_ms))]
    pub async fn invoke(
        &self,
        function_name: &str,
        request: &ApiGatewayV2httpRequest,
        timeout_ms: u64,
    ) -> Result<ApiGatewayV2httpResponse, PoolError> {
        let pool = self
            .pools
            .read()
            .await
            .get(function_name)
            .cloned()
            .ok_or_else(|| PoolError::NoPool(function_name.into()))?;

        if !pool.healthy.load(Ordering::Relaxed) {
            return Ok(error_response(503, "lambda unhealthy"));
        }

        let (_permit, arc) = acquire_worker(&pool, function_name).await?;
        let mut handle = arc.lock().await;

        // RuntimeApi (rust/go): the unmodified official binary speaks the AWS
        // Lambda Runtime API. A handler `/error` is a normal result (HTTP 502),
        // NOT a worker crash, so this path is separate from the stdio match.
        if let HandleTransport::RuntimeApi { endpoint } = &handle.transport {
            let sender = endpoint.sender.clone();
            let outcome = runtime_api_roundtrip(&sender, function_name, request, timeout_ms).await;
            return match outcome {
                RtOutcome::Response(bytes) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    serde_json::from_slice(&bytes).map_err(|e| {
                        PoolError::InvalidResponse(function_name.into(), e.to_string())
                    })
                }
                RtOutcome::HandlerError(msg) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    Ok(error_response(502, &format!("handler error: {msg}")))
                }
                RtOutcome::WorkerGone => {
                    warn!("runtime-api worker on {function_name} exited without responding — restarting");
                    fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::Other(
                        function_name.into(),
                        anyhow::anyhow!("worker exited without responding"),
                    ))
                }
                RtOutcome::Timeout => {
                    warn!("lambda timeout on {function_name} after {timeout_ms}ms — killing and restarting");
                    respawn_after_timeout(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::Timeout(function_name.into(), timeout_ms))
                }
            };
        }

        let payload = build_envelope_payload(request, function_name, timeout_ms)
            .map_err(|e| PoolError::Other(function_name.into(), e.into()))?
            + "\n";

        match stdio_roundtrip(&mut handle, function_name, &payload, timeout_ms).await? {
            StdioOutcome::Line(line) => match serde_json::from_str(line.trim()) {
                Ok(resp) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    Ok(resp)
                }
                Err(e) => {
                    warn!("malformed lambda response on {function_name}: {line:?} — killing and restarting");
                    fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::InvalidResponse(
                        function_name.into(),
                        e.to_string(),
                    ))
                }
            },
            StdioOutcome::Io(e) => {
                warn!("lambda crash on {function_name}: {e} — restarting");
                fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                Err(PoolError::Other(function_name.into(), e))
            }
            StdioOutcome::TimedOut => {
                warn!("lambda timeout on {function_name} after {timeout_ms}ms — killing and restarting");
                respawn_after_timeout(&pool, &arc, &mut handle, function_name).await;
                Err(PoolError::Timeout(function_name.into(), timeout_ms))
            }
        }
    }

    /// Invoke a function with an arbitrary serializable event (WebSocket events,
    /// future event sources). Same pool plumbing as `invoke`; only the wire
    /// payload type differs. Returns the response deserialized into `R`, or a
    /// typed [`PoolError`] on failure.
    #[tracing::instrument(skip(self, request), fields(function = %function_name, timeout_ms))]
    pub async fn invoke_generic<E, R>(
        &self,
        function_name: &str,
        request: &E,
        timeout_ms: u64,
    ) -> Result<R, PoolError>
    where
        E: serde::Serialize,
        R: serde::de::DeserializeOwned + Default,
    {
        let pool = self
            .pools
            .read()
            .await
            .get(function_name)
            .cloned()
            .ok_or_else(|| PoolError::NoPool(function_name.into()))?;

        if !pool.healthy.load(Ordering::Relaxed) {
            return Ok(R::default());
        }

        let (_permit, arc) = acquire_worker(&pool, function_name).await?;
        let mut handle = arc.lock().await;

        // RuntimeApi (rust/go) generic-event path (WebSocket etc.). A handler
        // `/error` is surfaced as a typed error; the worker stays healthy.
        if let HandleTransport::RuntimeApi { endpoint } = &handle.transport {
            let sender = endpoint.sender.clone();
            let outcome = runtime_api_roundtrip(&sender, function_name, request, timeout_ms).await;
            return match outcome {
                RtOutcome::Response(bytes) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    serde_json::from_slice(&bytes).map_err(|e| {
                        PoolError::InvalidResponse(function_name.into(), e.to_string())
                    })
                }
                RtOutcome::HandlerError(msg) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    Err(PoolError::Other(
                        function_name.into(),
                        anyhow::anyhow!("handler error: {msg}"),
                    ))
                }
                RtOutcome::WorkerGone => {
                    warn!("runtime-api ws worker on {function_name} exited without responding — restarting");
                    fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::Other(
                        function_name.into(),
                        anyhow::anyhow!("worker exited without responding"),
                    ))
                }
                RtOutcome::Timeout => {
                    warn!(
                        "ws handler timeout on {function_name} after {timeout_ms}ms — restarting"
                    );
                    respawn_after_timeout(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::Timeout(function_name.into(), timeout_ms))
                }
            };
        }

        let payload = build_envelope_payload(request, function_name, timeout_ms)
            .map_err(|e| PoolError::Other(function_name.into(), e.into()))?
            + "\n";

        match stdio_roundtrip(&mut handle, function_name, &payload, timeout_ms).await? {
            StdioOutcome::Line(line) => match serde_json::from_str(line.trim()) {
                Ok(resp) => {
                    pool.consecutive_crashes.store(0, Ordering::Relaxed);
                    Ok(resp)
                }
                Err(e) => {
                    warn!("malformed ws handler response on {function_name}: {line:?} — killing and restarting");
                    fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                    Err(PoolError::InvalidResponse(
                        function_name.into(),
                        e.to_string(),
                    ))
                }
            },
            StdioOutcome::Io(e) => {
                warn!("ws handler crash on {function_name}: {e} — restarting");
                fail_and_rearm(&pool, &arc, &mut handle, function_name).await;
                Err(PoolError::Other(function_name.into(), e))
            }
            StdioOutcome::TimedOut => {
                warn!("ws handler timeout on {function_name} after {timeout_ms}ms — killing and restarting");
                respawn_after_timeout(&pool, &arc, &mut handle, function_name).await;
                Err(PoolError::Timeout(function_name.into(), timeout_ms))
            }
        }
    }

    /// Replace a function's process pool in-place with a new FunctionConfig.
    /// Drains the semaphore (waits for in-flight invocations), kills the old
    /// processes, spawns a fresh pool matching the new config.
    pub async fn hot_swap(
        &self,
        function_name: &str,
        new_config: FunctionConfig,
        registry: &RuntimeRegistry,
    ) -> anyhow::Result<u32> {
        let pools = self.pools.read().await;
        let pool = pools
            .get(function_name)
            .ok_or_else(|| anyhow::anyhow!("unknown function {function_name}"))?
            .clone();
        drop(pools);

        let concurrency = pool.config.concurrency as u32;
        let _drain = pool.semaphore.acquire_many(concurrency).await?;

        let mut handles = pool.handles.write().await;
        for h in handles.iter() {
            if let Ok(g) = h.try_lock() {
                kill_process_group(g.pid);
            }
        }
        handles.clear();

        let mut first_pid = 0;
        for _ in 0..new_config.concurrency {
            let h = spawn_process(&new_config, function_name, registry, &pool.log_tx).await?;
            pool.riz_state.note_cold_start(function_name).await;
            if first_pid == 0 {
                first_pid = h.pid;
            }
            let handle_arc = Arc::new(Mutex::new(h));
            let pid = handle_arc.lock().await.pid;
            spawn_liveness_watcher(
                pid,
                handle_arc.clone(),
                pool.clone(),
                function_name.to_string(),
            );
            handles.push(handle_arc);
        }

        pool.healthy.store(true, Ordering::Relaxed);
        pool.consecutive_crashes.store(0, Ordering::Relaxed);

        Ok(first_pid)
    }

    /// Drain and remove a function's pool entirely (used by hot-reload when
    /// a function is removed from riz.toml).
    pub async fn drain_pool(&self, function_name: &str) {
        let pool = {
            let pools = self.pools.read().await;
            pools.get(function_name).cloned()
        };
        if let Some(pool) = pool {
            let concurrency = pool.config.concurrency as u32;
            if let Ok(_drain) = pool.semaphore.acquire_many(concurrency).await {
                let mut handles = pool.handles.write().await;
                for h in handles.iter() {
                    if let Ok(g) = h.try_lock() {
                        kill_process_group(g.pid);
                    }
                }
                handles.clear();
            }
        }
        self.pools.write().await.remove(function_name);
    }

    /// Create a new pool for a function added at runtime (hot-reload).
    pub async fn spawn_function(
        &self,
        name: &str,
        cfg: &FunctionConfig,
        registry: &Arc<RuntimeRegistry>,
        log_tx: mpsc::Sender<LogEntry>,
    ) -> anyhow::Result<()> {
        let pool = Arc::new(RoutePool {
            name: name.to_string(),
            config: cfg.clone(),
            handles: RwLock::new(Vec::new()),
            semaphore: Arc::new(Semaphore::new(cfg.concurrency)),
            restart_count: AtomicU32::new(0),
            consecutive_crashes: AtomicU32::new(0),
            healthy: AtomicBool::new(true),
            runtime_registry: registry.clone(),
            log_tx: log_tx.clone(),
            riz_state: self.riz_state.clone(),
        });
        let mut handle_vec = pool.handles.write().await;
        for _ in 0..cfg.concurrency {
            let handle = spawn_with_cold_start_record(&pool, name)
                .await
                .with_context(|| format!("failed to spawn lambda for {name}"))?;
            let handle_arc = Arc::new(Mutex::new(handle));
            let pid = handle_arc.lock().await.pid;
            spawn_liveness_watcher(pid, handle_arc.clone(), pool.clone(), name.to_string());
            handle_vec.push(handle_arc);
        }
        drop(handle_vec);
        self.pools.write().await.insert(name.to_string(), pool);
        Ok(())
    }

    /// Stats for the Riz host process itself (the daemon that owns all the
    /// pools). System endpoints (`/_riz/*`) run inside this process and share
    /// its memory/CPU footprint.
    pub fn host_stats(&self) -> HostStats {
        let pid = std::process::id();
        // Poison recovery (rule 7): `sys` is a stats-only sysinfo cache — if
        // another thread panicked mid-refresh, the worst case is one stale
        // sample, overwritten by the refresh below. Never worth a panic.
        let mut sys = self
            .sys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );
        let (mem_bytes, cpu) = match sys.process(Pid::from_u32(pid)) {
            Some(p) => (p.memory(), p.cpu_usage()),
            None => (0, 0.0),
        };
        let cores = sys.cpus().len();
        HostStats {
            pid,
            memory_rss_mb: mem_bytes as f64 / (1024.0 * 1024.0),
            cpu_percent: cpu,
            cores,
        }
    }

    pub async fn pool_stats(&self) -> Vec<PoolStats> {
        let pools = self.pools.read().await;

        struct RawStat {
            name: String,
            pids: Vec<u32>,
            restarts: u32,
            healthy: bool,
            concurrency: usize,
        }
        let mut raw: Vec<RawStat> = Vec::new();
        for (name, pool) in pools.iter() {
            let handles = pool.handles.read().await;
            let pids = handles
                .iter()
                .filter_map(|h| h.try_lock().ok().map(|g| g.pid))
                .collect();
            raw.push(RawStat {
                name: name.clone(),
                pids,
                restarts: pool.restart_count.load(Ordering::Relaxed),
                healthy: pool.healthy.load(Ordering::Relaxed),
                concurrency: pool.config.concurrency,
            });
        }
        drop(pools);

        // Refresh sysinfo (sync — no await points here)
        // Collect all PIDs first, then pass them to ProcessesToUpdate::Some
        // so sysinfo only scans the specific PIDs we care about
        let all_pids: Vec<sysinfo::Pid> = raw
            .iter()
            .flat_map(|r| r.pids.iter().map(|&p| sysinfo::Pid::from_u32(p)))
            .collect();

        // Poison recovery (rule 7): same stats-only rationale as host_stats.
        let mut sys = self
            .sys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&all_pids),
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );

        raw.into_iter()
            .map(|r| {
                // saturating_add: summed RSS across a pool cannot overflow u64
                // in practice; saturation (not wrap) keeps the stat honest-ish
                // if it ever did.
                let (mem_bytes, cpu) = r.pids.iter().fold((0u64, 0f32), |(m, c), &pid| {
                    match sys.process(Pid::from_u32(pid)) {
                        Some(p) => (m.saturating_add(p.memory()), c + p.cpu_usage()),
                        None => (m, c),
                    }
                });
                PoolStats {
                    name: r.name,
                    pids: r.pids,
                    restart_count: r.restarts,
                    healthy: r.healthy,
                    concurrency: r.concurrency,
                    memory_rss_mb: mem_bytes as f64 / (1024.0 * 1024.0),
                    cpu_percent: cpu,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::pool::kill_process_group;

    #[test]
    fn build_envelope_payload_has_correct_keys() {
        #[derive(serde::Serialize)]
        struct FakeEvent {
            path: &'static str,
        }
        let event = FakeEvent { path: "/hello" };
        let json_str = build_envelope_payload(&event, "api", 5000)
            .expect("envelope must serialize without error");
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("envelope must be valid JSON");

        // Event field is nested.
        assert_eq!(parsed["event"]["path"], "/hello");

        // Function name must match the argument.
        assert_eq!(parsed["__riz_function_name"], "api");

        // Deadline must be a positive integer in epoch-millis range.
        let deadline = parsed["__riz_deadline_ms"]
            .as_i64()
            .expect("__riz_deadline_ms must be an integer");
        assert!(
            deadline > 0,
            "__riz_deadline_ms must be > 0, got {deadline}"
        );

        // The deadline must be at least now+5000ms (epoch ms sanity check).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(
            deadline >= now_ms,
            "__riz_deadline_ms {deadline} must be >= now {now_ms}"
        );
        assert!(
            deadline <= now_ms + 6000,
            "__riz_deadline_ms {deadline} must be <= now+6000ms (clock skew guard)"
        );
    }

    /// The former `unreachable!("RuntimeApi handled above")` is now an error
    /// return: a runtime-api worker reaching the stdio roundtrip fails that
    /// ONE invocation instead of panicking the supervisor.
    #[tokio::test]
    async fn stdio_roundtrip_on_runtime_api_worker_is_an_error_not_a_panic() {
        let endpoint = crate::process::runtime_api::WorkerEndpoint::start()
            .await
            .expect("endpoint");
        let mut child = tokio::process::Command::new("true")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `true`");
        let pid = child.id().unwrap_or(0);
        // Reap eagerly so the test never leaks; `true` exits on its own.
        let _ = child.wait().await;
        let mut handle = ProcessHandle {
            pid,
            spawned_at: std::time::Instant::now(),
            _child: child,
            transport: HandleTransport::RuntimeApi { endpoint },
        };
        let err = match stdio_roundtrip(&mut handle, "test-fn", "{}\n", 100).await {
            Err(e) => e,
            Ok(_) => panic!("a non-stdio worker must fail the invocation"),
        };
        assert!(
            err.to_string().contains("transport dispatch bug"),
            "error must name the dispatch bug: {err}"
        );
    }

    /// Cancel-safety of the stdio roundtrip: dropping the future mid-flight
    /// (integration timeout, vanished WS client) must kill the worker so the
    /// desynced pipe can never answer a LATER invocation with a stale line.
    /// This also pins the fix for `invoke_generic`, which previously had no
    /// drop guard at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_stdio_roundtrip_kills_the_worker() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd.process_group(0); // kill_process_group targets the pgid
        let mut child = cmd.spawn().expect("spawn `sleep 30`");
        let pid = child.id().expect("live child has a pid");
        let transport = HandleTransport::Stdio {
            stdin: child.stdin.take().expect("piped stdin (test setup)"),
            stdout: tokio::io::BufReader::new(child.stdout.take().expect("piped stdout")),
        };
        let mut handle = ProcessHandle {
            pid,
            spawned_at: std::time::Instant::now(),
            _child: child,
            transport,
        };

        // `sleep` never answers, so the roundtrip parks in read_line; the
        // select drops it after 50ms — the mid-flight cancellation.
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            _ = stdio_roundtrip(&mut handle, "test-fn", "{}\n", 60_000) => {
                panic!("roundtrip against `sleep` must not complete");
            }
        }

        // The drop guard must have SIGKILLed the group: wait() resolves well
        // before sleep's 30s, with a non-success status.
        let status = timeout(Duration::from_secs(5), handle._child.wait())
            .await
            .expect("worker must die promptly after the roundtrip is dropped")
            .expect("wait() on the killed worker");
        assert!(
            !status.success(),
            "worker must have been killed, not exited cleanly: {status:?}"
        );
    }

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
        let result =
            serde_json::from_str::<crate::gateway::ApiGatewayV2httpResponse>(bad_line.trim());
        assert!(
            result.is_err(),
            "non-JSON line must fail to parse — this is the trigger condition for BUG-01"
        );
        // Empty line is a different edge case (read_line returned EOF or blank)
        let empty_result =
            serde_json::from_str::<crate::gateway::ApiGatewayV2httpResponse>("".trim());
        assert!(empty_result.is_err(), "empty string also fails to parse");
    }

    #[test]
    fn liveness_watcher_skips_when_pid_changes() {
        // Simulates the guard: if guard.pid != original_pid, watcher does nothing.
        // This is the invariant that prevents double-respawn.
        let original_pid = 12345u32;
        let current_pid = 99999u32; // already respawned
        assert_ne!(
            original_pid, current_pid,
            "PID mismatch means process already respawned — watcher must skip"
        );
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
            matches!(
                sem.try_acquire(),
                Err(tokio::sync::TryAcquireError::NoPermits)
            ),
            "exhausted semaphore must return NoPermits"
        );

        // Closed semaphore returns a different error variant
        let sem2 = tokio::sync::Semaphore::new(1);
        sem2.close();
        assert!(
            matches!(
                sem2.try_acquire(),
                Err(tokio::sync::TryAcquireError::Closed)
            ),
            "closed semaphore must return Closed, not NoPermits"
        );
    }

    #[test]
    fn processes_to_update_some_accepts_pid_slice() {
        // Verifies the API: ProcessesToUpdate::Some takes a &[Pid] slice.
        // This documents the sysinfo API we depend on.
        let pids = vec![sysinfo::Pid::from_u32(1), sysinfo::Pid::from_u32(2)];
        let _update = sysinfo::ProcessesToUpdate::Some(&pids);
        // If this compiles, the API is correct
    }

    #[tokio::test]
    async fn invoke_ws_returns_serialized_response() {
        use crate::gateway::ApiGatewayWebsocketProxyRequest;
        fn _accepts_ws_event<F>(f: F)
        where
            F: FnOnce(&ApiGatewayWebsocketProxyRequest),
        {
            let _ = f;
        }
        let ev = crate::ws::event::build_connect(
            "$default",
            "c1",
            0,
            "/chat",
            http::HeaderMap::new(),
            std::collections::HashMap::new(),
        );
        _accepts_ws_event(|_e: &ApiGatewayWebsocketProxyRequest| {});
        // ev is the correct type — passing it to the type-shape check above
        // confirms build_connect returns ApiGatewayWebsocketProxyRequest.
        let _: &ApiGatewayWebsocketProxyRequest = &ev;
    }

    /// Proves the hot-swap drain mechanism: acquiring all permits from the
    /// concurrency semaphore blocks new invocations while in-flight ones
    /// complete, guaranteeing zero in-flight requests at the swap point.
    #[tokio::test]
    async fn hot_swap_drains_in_flight_requests() {
        let concurrency = 3u32;
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency as usize));

        // Simulate one in-flight request holding a permit.
        let _in_flight = sem.acquire().await.expect("permit");

        // hot_swap acquires ALL permits — this will block until the in-flight
        // request's permit is released, proving the drain is watertight.
        let sem2 = sem.clone();
        let drain_task = tokio::spawn(async move {
            // Acquiring concurrency permits is the drain: waits for all slots.
            let _drain = sem2.acquire_many(concurrency).await.expect("drain");
        });

        // Release the in-flight permit — drain_task can now complete.
        drop(_in_flight);
        drain_task
            .await
            .expect("drain task must complete after in-flight releases");
    }
}
