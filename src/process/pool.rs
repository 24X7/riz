use crate::config::FunctionConfig;
use crate::process::runtime::RuntimeRegistry;
use crate::state::{LogEntry, RizState};
use anyhow::Context;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};

pub(super) const CRASH_THRESHOLD: u32 = 5;

pub(super) struct ProcessHandle {
    pub(super) pid: u32,
    pub(super) stdin: ChildStdin,
    pub(super) stdout: BufReader<ChildStdout>,
    #[allow(dead_code)]
    pub(super) spawned_at: Instant,
    pub(super) _child: Child,
}

/// One pool per FUNCTION (not per route). All routes belonging to a
/// function share the pool's processes — matches AWS Lambda execution
/// environments where N routes can target the same Lambda.
pub(super) struct RoutePool {
    /// Function name (`api`, `users`) — used as the map key in
    /// ProcessManager.pools and as the cold_starts attribution key.
    #[allow(dead_code)]
    pub(super) name: String,
    pub(super) config: FunctionConfig,
    pub(super) handles: RwLock<Vec<Arc<Mutex<ProcessHandle>>>>,
    pub(super) semaphore: Arc<Semaphore>,
    pub(super) restart_count: AtomicU32,
    pub(super) consecutive_crashes: AtomicU32,
    pub(super) healthy: AtomicBool,
    pub(super) runtime_registry: Arc<RuntimeRegistry>,
    pub(super) log_tx: mpsc::Sender<LogEntry>,
    /// Shared RizState used to bump cold_starts on every successful spawn.
    pub(super) riz_state: Arc<RizState>,
}

/// Spawn a new process and immediately record a cold start against
/// `function_name`. Every spawn site should use this instead of calling
/// `spawn_process` + `note_cold_start` separately — makes it impossible to
/// forget the accounting step.
pub(super) async fn spawn_with_cold_start_record(
    pool: &Arc<RoutePool>,
    function_name: &str,
) -> anyhow::Result<ProcessHandle> {
    let handle = spawn_process(&pool.config, &pool.runtime_registry, &pool.log_tx).await?;
    pool.riz_state.note_cold_start(function_name).await;
    Ok(handle)
}

#[tracing::instrument(skip(cfg, registry, log_tx), fields(handler = ?cfg.handler, runtime = ?cfg.runtime))]
pub(super) async fn spawn_process(
    cfg: &FunctionConfig,
    registry: &RuntimeRegistry,
    log_tx: &mpsc::Sender<LogEntry>,
) -> anyhow::Result<ProcessHandle> {
    let runtime = registry.get(&cfg.runtime);
    tracing::debug!(runtime = runtime.name(), handler = ?cfg.handler, "spawning lambda process");
    let mut cmd = runtime.spawn_command(cfg);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        cmd.process_group(0);
        // SAFETY: apply_always_on_limits is async-signal-safe (setrlimit
        // is on the POSIX safe list). Runs after fork, before execve.
        // tokio::process::Command provides pre_exec inherently — no
        // CommandExt import needed.
        unsafe {
            cmd.pre_exec(crate::process::safety::apply_always_on_limits);
        }
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {:?}", cfg.handler))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    if let Some(stderr) = child.stderr.take() {
        // Tag stderr logs with the handler filename — best signal we have
        // about which function it came from at this layer.
        let tag = cfg
            .handler
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "lambda".into());
        let tx = log_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let _ = tx.try_send(LogEntry {
                    timestamp: std::time::SystemTime::now(),
                    level: "WARN".into(),
                    message: format!("stderr: {line}"),
                    route_key: Some(tag.clone()),
                });
            }
        });
    }

    Ok(ProcessHandle {
        pid,
        stdin,
        stdout,
        spawned_at: Instant::now(),
        _child: child,
    })
}

#[cfg(unix)]
#[tracing::instrument(fields(pid))]
pub fn kill_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
#[tracing::instrument(fields(pid))]
pub fn kill_process_group(_pid: u32) {}
