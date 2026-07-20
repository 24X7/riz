use crate::config::FunctionConfig;
use crate::process::runtime::{RuntimeRegistry, WorkerTransport};
use crate::process::runtime_api::WorkerEndpoint;
use crate::state::{LogEntry, RizState};
use anyhow::Context;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};

pub(super) const CRASH_THRESHOLD: u32 = 5;

/// Turn a process-spawn `io::Error` into an actionable message. A `NotFound`
/// means the runtime program isn't where we expected: for interpreter runtimes
/// (Bun/Node/Python) the interpreter isn't on PATH — name it and give an
/// install hint; for compiled runtimes (Rust/Go/Wasm) the handler artifact
/// hasn't been built. Everything else keeps the raw error with context.
fn spawn_error(
    e: std::io::Error,
    runtime: &crate::config::RuntimeKind,
    handler: &std::path::Path,
) -> anyhow::Error {
    use crate::config::RuntimeKind;
    if e.kind() != std::io::ErrorKind::NotFound {
        return anyhow::Error::new(e).context(format!(
            "failed to spawn {} handler {}",
            runtime.as_str(),
            handler.display()
        ));
    }
    match runtime {
        RuntimeKind::Bun => anyhow::anyhow!(
            "runtime 'bun' not found on PATH — the bun function needs it.\n  \
             Install: curl -fsSL https://bun.sh/install | bash   (then restart your shell)\n  \
             Check all runtimes with `riz doctor`."
        ),
        RuntimeKind::Node => anyhow::anyhow!(
            "runtime 'node' not found on PATH — the nodejs function needs it.\n  \
             Install Node.js from https://nodejs.org (or a version manager like nvm/fnm).\n  \
             Check all runtimes with `riz doctor`."
        ),
        RuntimeKind::Python => anyhow::anyhow!(
            "runtime 'python3' not found on PATH — the python function needs it.\n  \
             Install Python 3 from https://python.org or your OS package manager.\n  \
             Check all runtimes with `riz doctor`."
        ),
        RuntimeKind::Rust | RuntimeKind::Go => anyhow::anyhow!(
            "handler binary not found: {}\n  \
             Build it first — the {} artifact is missing (see the function's README), then `riz run`.",
            handler.display(),
            runtime.as_str()
        ),
        RuntimeKind::Wasm => anyhow::anyhow!(
            "wasm handler not found: {}\n  \
             Build it: cargo build --release --target wasm32-wasip1",
            handler.display()
        ),
    }
}

pub(super) struct ProcessHandle {
    pub(super) pid: u32,
    #[allow(dead_code)]
    pub(super) spawned_at: Instant,
    pub(super) _child: Child,
    pub(super) transport: HandleTransport,
}

/// How riz exchanges events/responses with this worker child.
pub(super) enum HandleTransport {
    /// bun/node/python: riz writes a line-JSON envelope to stdin and reads the
    /// response line from stdout.
    Stdio {
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    /// rust/go: the child is an unmodified official AWS runtime client polling
    /// its per-worker AWS Runtime API endpoint. riz hands invocations to (and
    /// receives responses from) the endpoint.
    RuntimeApi { endpoint: WorkerEndpoint },
}

/// What `spawn_process` provisions BEFORE the child starts, keyed by
/// transport. Carrying the endpoint inside the variant (instead of an
/// `Option` reunited with the transport kind after spawn) lets the type
/// system prove "a runtime-api worker always has its endpoint" — there is
/// no absent case left to `expect` away (rule 5: encode the invariant).
enum ProvisionedTransport {
    Stdio,
    RuntimeApi { endpoint: WorkerEndpoint },
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
    /// Requests rejected because the pool was at its concurrency limit
    /// (load-shed events). A rising rate is the saturation signal that says
    /// "raise concurrency or add instances".
    pub(super) admission_rejected: AtomicU64,
    pub(super) healthy: AtomicBool,
    pub(super) runtime_registry: Arc<RuntimeRegistry>,
    pub(super) log_tx: mpsc::Sender<LogEntry>,
    /// Shared RizState used to bump cold_starts on every successful spawn.
    pub(super) riz_state: Arc<RizState>,
    /// Broker env (`RIZ_BROKER_SOCK`/`TOKEN`/`TIMEOUT_MS`) stamped onto every
    /// worker this pool spawns. Empty for grantless functions — the worker
    /// then has no broker client and answers capability calls `denied`.
    pub(super) broker_env: Vec<(String, String)>,
}

/// Spawn a new process and immediately record a cold start against
/// `function_name`. Every spawn site should use this instead of calling
/// `spawn_process` + `note_cold_start` separately — makes it impossible to
/// forget the accounting step.
pub(super) async fn spawn_with_cold_start_record(
    pool: &Arc<RoutePool>,
    function_name: &str,
) -> anyhow::Result<ProcessHandle> {
    let handle = spawn_process(
        &pool.config,
        function_name,
        &pool.runtime_registry,
        &pool.log_tx,
        &pool.broker_env,
    )
    .await?;
    pool.riz_state.note_cold_start(function_name).await;
    Ok(handle)
}

/// The only daemon-env vars a worker inherits after `env_clear()`. Everything
/// else — DSNs, API keys, the daemon's own secrets — stays in the daemon.
/// This is deliberately conservative: a runtime that provably needs another
/// var earns it here (with the all-six e2e smoke as the proof), never a
/// blanket passthrough. A function's own secrets go through `[function.X.env]`.
const SCRUBBED_ENV_ALLOWLIST: &[&str] = &[
    // Toolchain + resolution: find the interpreter/binary and its caches.
    "PATH",
    "HOME",
    "TMPDIR",
    // Locale + timezone: correct text handling in node/python/bun.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TERM",
    // TLS roots for a handler's own outbound HTTPS (script runtimes are not
    // WASI-sandboxed; wasm guests reach the network only through the broker).
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // Egress proxy configuration, both cases.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // riz's OWN control vars — non-secret, and passed by exact name so a
    // user's `RIZ_`-prefixed DSN or secret is NOT auto-forwarded (that is the
    // canary test's guarantee). RIZ_TEST_BASE_URL points a WebSocket handler
    // at the live @connections endpoint (an ephemeral port under test).
    "RIZ_TEST_BASE_URL",
];

/// Copy the allowlisted base vars that are present in the daemon env onto the
/// child command. Called right after `env_clear()`.
fn apply_base_env(cmd: &mut tokio::process::Command) {
    for &key in SCRUBBED_ENV_ALLOWLIST {
        if let Some(val) = std::env::var_os(key) {
            cmd.env(key, val);
        }
    }
}

#[tracing::instrument(skip(cfg, registry, log_tx, broker_env), fields(handler = ?cfg.handler, runtime = ?cfg.runtime))]
pub(super) async fn spawn_process(
    cfg: &FunctionConfig,
    function_name: &str,
    registry: &RuntimeRegistry,
    log_tx: &mpsc::Sender<LogEntry>,
    broker_env: &[(String, String)],
) -> anyhow::Result<ProcessHandle> {
    let runtime = registry.get(&cfg.runtime);
    let transport_kind = runtime.transport();
    tracing::debug!(runtime = runtime.name(), handler = ?cfg.handler, ?transport_kind, "spawning lambda process");
    let mut cmd = runtime.spawn_command(cfg);

    // SCRUB THE ENVIRONMENT. A worker must not inherit the daemon's full env —
    // that is where resource DSNs and other secrets live, and secrets exist in
    // exactly one process (the daemon). `spawn_command` set only program+argv
    // (the adapters never touch env), so clearing here is safe, and it MUST
    // come before every `.env()` below — including the AWS_LAMBDA_* vars a
    // RuntimeApi worker needs, which are re-added explicitly.
    cmd.env_clear();
    apply_base_env(&mut cmd);

    // Broker env for a granted wasm worker: its per-function token + socket.
    // Empty for grantless functions. Set before the per-function env below so
    // a function's own `[env]` can never shadow the broker control vars.
    for (key, value) in broker_env {
        cmd.env(key, value);
    }

    // Per-function `[function.X.env]` lands FIRST so riz's own variables
    // (AWS_LAMBDA_*, _HANDLER, runtime internals set below) win on conflict.
    for (key, value) in &cfg.env {
        cmd.env(key, value);
    }

    // RuntimeApi (rust/go): provision a per-worker AWS Lambda Runtime API
    // endpoint and expose it to the unmodified official runtime client via the
    // standard AWS env vars. The event is delivered over HTTP, not stdin.
    let provisioned = match transport_kind {
        WorkerTransport::Stdio => ProvisionedTransport::Stdio,
        WorkerTransport::RuntimeApi => {
            let endpoint = WorkerEndpoint::start().await?;
            cmd.env("AWS_LAMBDA_RUNTIME_API", endpoint.addr.to_string())
                .env("AWS_LAMBDA_FUNCTION_NAME", function_name)
                .env("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST")
                .env(
                    "AWS_LAMBDA_FUNCTION_MEMORY_SIZE",
                    cfg.memory_mb
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "128".into()),
                )
                .env("_HANDLER", cfg.handler.to_string_lossy().to_string());
            ProvisionedTransport::RuntimeApi { endpoint }
        }
    };

    // stdio: stdin+stdout are the event channel. runtime-api: stdin is unused
    // (events arrive over HTTP) and stdout is captured as logs.
    match &provisioned {
        ProvisionedTransport::Stdio => {
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
        }
        ProvisionedTransport::RuntimeApi { .. } => {
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped());
        }
    }
    cmd.stderr(std::process::Stdio::piped())
        // Safety net: if a ProcessHandle is dropped without going through the
        // explicit drain/kill path (e.g. a pool torn down at shutdown, a handle
        // dropped on respawn, or an integration test ending while a slow handler
        // is still running), reap the child instead of orphaning it. The graceful
        // drain path still kills explicitly first; SIGKILL on an already-exited
        // child is a no-op. Eliminates nextest "leaky" reports + orphaned workers.
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        cmd.process_group(0);
        // Capture per-function opt-in caps as locals so the pre_exec
        // closure can move them across fork without holding a reference
        // to the FunctionConfig (which doesn't live across fork).
        let memory_mb = cfg.memory_mb;
        let cpu_time_secs = cfg.cpu_time_secs;
        let allowed_paths = cfg.allowed_paths.clone();
        // SAFETY: apply_always_on_limits + apply_per_function_limits +
        // apply_filesystem_allowlist + apply_seccomp_blocklist are
        // async-signal-safe enough for pre_exec — they make syscalls
        // (setrlimit, prctl, landlock, seccomp). The landlock and seccompiler
        // crates allocate internally; widespread real-world use in pre_exec
        // (systemd, container runtimes) attests this is safe in practice on
        // modern glibc/musl. seccomp is applied LAST so the earlier setup
        // syscalls are never filtered.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(move || {
                crate::process::safety::apply_always_on_limits()?;
                crate::process::safety::apply_per_function_limits(memory_mb, cpu_time_secs)?;
                if let Some(paths) = &allowed_paths {
                    crate::process::safety::apply_filesystem_allowlist(paths)?;
                }
                crate::process::safety::apply_seccomp_blocklist()?;
                Ok(())
            });
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_error(e, &cfg.runtime, &cfg.handler))?;

    let pid = child.id().unwrap_or(0);

    // Tag logs with the handler filename — best signal we have at this layer.
    let tag = cfg
        .handler
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "lambda".into());

    if let Some(stderr) = child.stderr.take() {
        tail_to_logs(stderr, tag.clone(), "stderr", log_tx.clone());
    }

    let transport = match provisioned {
        ProvisionedTransport::Stdio => wire_stdio_transport(&mut child)?,
        ProvisionedTransport::RuntimeApi { endpoint } => {
            // The runtime-api child uses HTTP for events; its stdout is just
            // handler logs (e.g. println!/fmt.Println) — tail it like stderr.
            if let Some(stdout) = child.stdout.take() {
                tail_to_logs(stdout, tag, "stdout", log_tx.clone());
            }
            HandleTransport::RuntimeApi { endpoint }
        }
    };

    Ok(ProcessHandle {
        pid,
        spawned_at: Instant::now(),
        _child: child,
        transport,
    })
}

/// Take the child's piped stdin/stdout as the stdio event channel.
///
/// The `Command` was configured with both pipes a few lines above, so
/// `take()` returning `None` means the pipe wiring failed. A worker without
/// its event channel is a FAILED SPAWN — surfaced as `Err` into the caller's
/// existing spawn-error path (unhealthy accounting, 503s), never a panic
/// (rule 7). The caller drops the `Child` on error; `kill_on_drop(true)`
/// reaps it.
fn wire_stdio_transport(child: &mut Child) -> anyhow::Result<HandleTransport> {
    let stdin = child
        .stdin
        .take()
        .context("stdio worker spawned without a piped stdin — pipe wiring failed")?;
    let stdout = child
        .stdout
        .take()
        .context("stdio worker spawned without a piped stdout — pipe wiring failed")?;
    Ok(HandleTransport::Stdio {
        stdin,
        stdout: BufReader::new(stdout),
    })
}

/// Cap on one buffered log line from a worker stream (rule 3: a worker
/// spewing an endless line with no newline must not balloon host memory).
/// Longer lines are forwarded truncated at the cap, marked, and the rest of
/// the line up to the next newline is discarded.
const MAX_LOG_LINE_BYTES: usize = 8 * 1024;

/// Tail a child stream (stdout/stderr) into the log channel, line by line.
///
/// Per-line memory is capped at [`MAX_LOG_LINE_BYTES`]; work per loop
/// iteration is bounded by the `BufReader`'s internal buffer (rule 2). The
/// task exits when the stream reaches EOF or errors — i.e. when the worker
/// dies — so its lifetime is tied to the child's.
fn tail_to_logs<R>(stream: R, tag: String, which: &'static str, tx: mpsc::Sender<LogEntry>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut line: Vec<u8> = Vec::new();
        let mut truncated = false;
        loop {
            // fill_buf exposes the reader's internal buffer without copying;
            // consume() advances past the bytes we scanned.
            let (advance, saw_newline) = {
                let chunk = match reader.fill_buf().await {
                    Ok([]) => break, // EOF — the worker closed the stream
                    Ok(chunk) => chunk,
                    Err(_) => break, // stream error — worker died; stop tailing
                };
                let newline_at = chunk.iter().position(|&b| b == b'\n');
                let upto = newline_at.unwrap_or(chunk.len());
                let room = MAX_LOG_LINE_BYTES.saturating_sub(line.len());
                let take = upto.min(room);
                if let Some(head) = chunk.get(..take) {
                    line.extend_from_slice(head);
                }
                if take < upto {
                    truncated = true; // over the cap — drop the line's excess
                }
                (
                    newline_at.map_or(chunk.len(), |i| i.saturating_add(1)),
                    newline_at.is_some(),
                )
            };
            reader.consume(advance);
            if saw_newline {
                emit_log_line(&mut line, &mut truncated, &tag, which, &tx);
            }
        }
        // Trailing partial line (worker exited without a final newline).
        emit_log_line(&mut line, &mut truncated, &tag, which, &tx);
    });
}

/// Send one buffered line (if non-blank) to the log channel, then reset the
/// buffer and truncation flag for the next line.
fn emit_log_line(
    line: &mut Vec<u8>,
    truncated: &mut bool,
    tag: &str,
    which: &'static str,
    tx: &mpsc::Sender<LogEntry>,
) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if !text.is_empty() {
        let marker = if *truncated { " …[truncated]" } else { "" };
        // Full channel = the log consumer is behind. Dropping the line IS the
        // backpressure policy (bounded channel, rule 3) — logs are lossy
        // telemetry, never worth stalling the tail task over.
        let _ = tx.try_send(LogEntry {
            timestamp: std::time::SystemTime::now(),
            level: "WARN".into(),
            message: format!("{which}: {text}{marker}"),
            route_key: Some(tag.to_string()),
        });
    }
    line.clear();
    *truncated = false;
}

#[cfg(unix)]
#[tracing::instrument(fields(pid))]
pub fn kill_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    // Result explicitly discarded (rule 7): ESRCH — the group is already
    // gone — is the expected benign failure on every double-kill path
    // (drop guard after an explicit kill, respawn racing the liveness
    // watcher). There is nothing to recover; the caller's respawn logic
    // does not depend on this signal landing.
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
#[tracing::instrument(fields(pid))]
pub fn kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeKind;
    use std::path::Path;

    #[test]
    fn spawn_error_missing_interpreter_names_binary_and_install_hint() {
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let msg = format!(
            "{}",
            spawn_error(e, &RuntimeKind::Bun, Path::new("./index.ts"))
        );
        assert!(msg.contains("bun"), "names the runtime binary: {msg}");
        assert!(msg.contains("PATH"), "explains it's not on PATH: {msg}");
        assert!(msg.contains("bun.sh"), "gives an install hint: {msg}");
        assert!(msg.contains("riz doctor"), "points at doctor: {msg}");
    }

    #[test]
    fn spawn_error_missing_compiled_artifact_says_build_it() {
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let msg = format!(
            "{}",
            spawn_error(e, &RuntimeKind::Rust, Path::new("./target/release/hello"))
        );
        assert!(
            msg.contains("handler binary not found"),
            "names the miss: {msg}"
        );
        assert!(msg.contains("Build it"), "tells the user to build: {msg}");
        assert!(msg.contains("hello"), "names the artifact path: {msg}");
    }

    #[test]
    fn spawn_error_other_kind_keeps_generic_context() {
        let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let msg = format!(
            "{}",
            spawn_error(e, &RuntimeKind::Python, Path::new("./app.py"))
        );
        assert!(
            msg.contains("failed to spawn"),
            "generic context kept: {msg}"
        );
    }

    #[test]
    fn scrub_allowlist_excludes_secrets_and_dsns() {
        // The allowlist is a passthrough set for non-secret toolchain/locale
        // vars; a DSN or secret-shaped name must never be on it (those reach a
        // worker only via the explicit `[function.X.env]` escape hatch).
        assert!(SCRUBBED_ENV_ALLOWLIST.contains(&"PATH"));
        assert!(!SCRUBBED_ENV_ALLOWLIST.contains(&"RIZ_PG_MAIN_DSN"));
        assert!(!SCRUBBED_ENV_ALLOWLIST.contains(&"RIZ_SECRET_CANARY"));
        assert!(!SCRUBBED_ENV_ALLOWLIST.contains(&"DATABASE_URL"));
        // riz's own control vars are allowed by EXACT name, never a RIZ_*
        // wildcard — so a user's RIZ_-prefixed secret is not swept in.
        assert!(SCRUBBED_ENV_ALLOWLIST.contains(&"RIZ_TEST_BASE_URL"));
        assert!(SCRUBBED_ENV_ALLOWLIST
            .iter()
            .all(|v| *v != "RIZ_BROKER_TOKEN"));
    }

    /// A stdio child spawned WITHOUT piped stdin must surface as a spawn
    /// error (failed pipe wiring), not a panic — the supervisor treats it
    /// exactly like any other failed spawn.
    #[tokio::test]
    async fn wire_stdio_transport_without_pipes_is_an_error_not_a_panic() {
        let mut child = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `true`");
        let err = match wire_stdio_transport(&mut child) {
            Err(e) => e,
            Ok(_) => panic!("un-piped child must not wire a stdio transport"),
        };
        assert!(
            err.to_string().contains("pipe wiring failed"),
            "error must name the failure: {err}"
        );
        // Result explicitly discarded: the child is `true` and exits on its
        // own; wait() just reaps it so the test leaves no process behind.
        let _ = child.wait().await;
    }

    /// Happy path: piped stdin/stdout wire into a Stdio transport.
    #[tokio::test]
    async fn wire_stdio_transport_with_pipes_succeeds() {
        let mut child = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `true`");
        let transport = wire_stdio_transport(&mut child).expect("pipes are wired");
        assert!(matches!(transport, HandleTransport::Stdio { .. }));
        let _ = child.wait().await;
    }

    /// Rule 3: a worker emitting one endless line (no newline) must not grow
    /// host memory past the cap — the line arrives truncated and marked.
    #[tokio::test]
    async fn tail_to_logs_caps_runaway_lines() {
        let (tx, mut rx) = mpsc::channel::<crate::state::LogEntry>(16);
        let giant = vec![b'a'; MAX_LOG_LINE_BYTES * 4]; // 32 KiB, no newline
        tail_to_logs(std::io::Cursor::new(giant), "t".into(), "stdout", tx);
        let entry = rx.recv().await.expect("one truncated line at EOF");
        assert!(
            entry.message.contains("[truncated]"),
            "over-cap line must be marked truncated"
        );
        assert!(
            entry.message.len() < MAX_LOG_LINE_BYTES + 64,
            "buffered line must be capped near MAX_LOG_LINE_BYTES, got {}",
            entry.message.len()
        );
        assert!(rx.recv().await.is_none(), "exactly one line is emitted");
    }

    /// Normal multi-line output passes through unmodified, one entry per
    /// line, blanks skipped — the pre-cap behavior.
    #[tokio::test]
    async fn tail_to_logs_passes_normal_lines_through() {
        let (tx, mut rx) = mpsc::channel::<crate::state::LogEntry>(16);
        let stream = b"hello\n\n  \nworld\n".to_vec();
        tail_to_logs(std::io::Cursor::new(stream), "t".into(), "stderr", tx);
        let first = rx.recv().await.expect("first line");
        assert_eq!(first.message, "stderr: hello");
        let second = rx.recv().await.expect("second line (blanks skipped)");
        assert_eq!(second.message, "stderr: world");
        assert!(rx.recv().await.is_none());
    }

    /// A trailing line without a final newline (worker died mid-line) is
    /// still delivered.
    #[tokio::test]
    async fn tail_to_logs_flushes_partial_line_at_eof() {
        let (tx, mut rx) = mpsc::channel::<crate::state::LogEntry>(16);
        tail_to_logs(
            std::io::Cursor::new(b"partial".to_vec()),
            "t".into(),
            "stdout",
            tx,
        );
        let entry = rx.recv().await.expect("partial line at EOF");
        assert_eq!(entry.message, "stdout: partial");
    }
}
