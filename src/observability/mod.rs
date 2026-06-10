//! Observability: an ISOLATED telemetry child process + a bounded, non-blocking
//! host emitter (phase 2a).
//!
//! The host emits through [`TelemetryHandle::emit`], a non-blocking `try_send`
//! on a bounded channel. If the queue is full (child slow/stalled) or closed
//! (drain task gone / child dead) the event is DROPPED and a counter
//! incremented — `emit` never awaits, never blocks, never fails the request
//! path. A [`TelemetrySupervisor`] owns the child: it resolves the exe (the same
//! `RIZ_HOST_BIN` override the WASM host uses, so it works under nextest),
//! spawns `riz __telemetry <sink>`, drains the channel to the child's stdin, and
//! respawns the child with bounded backoff if it exits.
//!
//! Telemetry being slow or crashed can therefore add neither latency nor failure
//! to serving requests.

pub mod ipc;
pub mod otel;
pub mod process;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use ipc::TelemetryEvent;

/// Min/max backoff between child respawn attempts.
const RESPAWN_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Clone-able host-side emitter. Cheap to clone (an `Arc` + a channel sender).
#[derive(Clone)]
pub struct TelemetryHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    /// `None` for a disabled handle (every emit drops).
    tx: Option<mpsc::Sender<TelemetryEvent>>,
    dropped: AtomicU64,
}

impl TelemetryHandle {
    /// A no-op handle: every `emit` is a drop. Used when
    /// `[telemetry].enabled = false` so call sites stay unconditional.
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(HandleInner {
                tx: None,
                dropped: AtomicU64::new(0),
            }),
        }
    }

    fn from_sender(tx: mpsc::Sender<TelemetryEvent>) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                tx: Some(tx),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    /// Emit a telemetry event. NON-BLOCKING and infallible from the caller's
    /// view: a full or closed queue drops the event and bumps the drop counter.
    /// Never awaits, never blocks, never returns an error.
    pub fn emit(&self, ev: TelemetryEvent) {
        match &self.inner.tx {
            Some(tx) => {
                if tx.try_send(ev).is_err() {
                    // Full (child slow/stalled) or Closed (drain gone): drop.
                    self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            None => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Total number of events dropped (overflow or closed channel).
    // Exercised by tests/telemetry_process_isolation.rs; the lib's dead-code
    // lint can't see the integration-test crate from the bin target.
    #[allow(dead_code)]
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Test seam: a handle whose bounded channel has the given capacity and NO
    /// drain running, so it saturates immediately. Proves `emit` never blocks.
    #[doc(hidden)]
    #[allow(dead_code)] // integration-test seam (see dropped() above)
    pub fn for_test_stalled(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        // Keep the receiver alive but never drain it: the channel is "stalled".
        // Leak it so it isn't dropped (which would mark the channel Closed and
        // change the failure mode from Full to Closed — both drop, but we want
        // to exercise the Full path specifically).
        Box::leak(Box::new(rx));
        Self::from_sender(tx)
    }
}

/// Resolve the path to the real `riz` binary for spawning the telemetry child.
/// Mirrors `process::wasm`: honour `RIZ_HOST_BIN` first (load-bearing under
/// `cargo nextest`, where `current_exe()` is the test runner), then
/// `current_exe()`, then a `"riz"` PATH fallback.
fn resolve_exe() -> PathBuf {
    std::env::var_os("RIZ_HOST_BIN")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("riz"))
}

/// Owns the isolated telemetry child: spawns it, drains the bounded channel to
/// its stdin, and respawns with bounded backoff if it exits.
// child_pid/task back child_pid()/shutdown(), which are reserved for graceful-
// shutdown wiring + health introspection and exercised by integration tests;
// dead-code lint can't see those from the bin target.
#[allow(dead_code)]
pub struct TelemetrySupervisor {
    handle: TelemetryHandle,
    /// Shared slot holding the current child's PID (for tests / health).
    child_pid: Arc<Mutex<Option<u32>>>,
    /// The drain+supervise task. Aborted on shutdown.
    task: tokio::task::JoinHandle<()>,
}

/// Where the telemetry child sends events: either an OTLP/HTTP collector
/// endpoint (2b export path) or, when no endpoint is configured, the sink file
/// (the 2a seam, also used by tests).
#[derive(Clone, Debug)]
pub struct ExportTarget {
    /// OTLP/HTTP collector base (e.g. `http://localhost:4318`). `None` => append
    /// JSON lines to the sink file instead of exporting.
    pub endpoint: Option<String>,
    /// Headers attached to every OTLP export POST (auth tokens, `dd-api-key`, …).
    pub headers: BTreeMap<String, String>,
}

impl ExportTarget {
    /// No endpoint: the child appends JSON lines to its sink file.
    #[allow(dead_code)] // used by integration tests + the disabled-telemetry path
    pub fn sink_only() -> Self {
        Self {
            endpoint: None,
            headers: BTreeMap::new(),
        }
    }
}

/// Env vars the supervisor sets on the `__telemetry` child to convey the OTLP
/// export target. Argv stays `__telemetry <sink>` (the sink is still the
/// fallback when no endpoint is set), and these add the exporter config.
const ENV_ENDPOINT: &str = "RIZ_TELEMETRY_ENDPOINT";
const ENV_HEADERS: &str = "RIZ_TELEMETRY_HEADERS";

impl TelemetrySupervisor {
    /// Spawn the supervisor and the first telemetry child. `sink` is the file
    /// the child appends events to when no endpoint is configured; `capacity`
    /// bounds the emit channel; `target` selects sink-file vs OTLP export.
    pub fn spawn(sink: &Path, capacity: usize, target: ExportTarget) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<TelemetryEvent>(capacity.max(1));
        let handle = TelemetryHandle::from_sender(tx);
        let child_pid = Arc::new(Mutex::new(None));

        let sink = sink.to_path_buf();
        let pid_slot = child_pid.clone();
        let task = tokio::spawn(async move {
            supervise_loop(rx, sink, target, pid_slot).await;
        });

        Ok(Self {
            handle,
            child_pid,
            task,
        })
    }

    /// A clone-able emitter for this supervisor's channel.
    pub fn handle(&self) -> TelemetryHandle {
        self.handle.clone()
    }

    /// The current child's OS PID, if a child is running. Safe to call from an
    /// async context: uses a non-blocking lock and reports `None` if the slot is
    /// momentarily contended (e.g. mid-respawn).
    #[allow(dead_code)] // health introspection; exercised by integration tests
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid.try_lock().ok().and_then(|g| *g)
    }

    /// Stop the supervisor and its child. Aborting the task drops the child's
    /// stdin and the `Child` handle, which closes the pipe and lets the worker
    /// exit on EOF.
    #[allow(dead_code)] // reserved for graceful-shutdown wiring; tested directly
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// The drain + respawn loop. Runs until the channel is closed (all handles
/// dropped) or the task is aborted.
async fn supervise_loop(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    sink: PathBuf,
    target: ExportTarget,
    pid_slot: Arc<Mutex<Option<u32>>>,
) {
    let exe = resolve_exe();
    let mut backoff = RESPAWN_BACKOFF_MIN;
    // Serialize the export target into env once; re-applied to every respawn.
    let headers_json = serde_json::to_string(&target.headers).unwrap_or_else(|_| "{}".into());

    loop {
        // Spawn a fresh child.
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("__telemetry")
            .arg(&sink)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(ep) = &target.endpoint {
            cmd.env(ENV_ENDPOINT, ep);
            cmd.env(ENV_HEADERS, &headers_json);
        }
        let mut child = match cmd.spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry: spawn failed; backing off");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
                continue;
            }
        };

        *pid_slot.lock().await = child.id();
        // Successful spawn resets the backoff.
        backoff = RESPAWN_BACKOFF_MIN;

        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let _ = child.kill().await;
                continue;
            }
        };

        // Drain the channel into the child's stdin until the child dies or the
        // channel closes. The channel is the host's only coupling to the child;
        // a stalled child just lets the bounded channel fill and `emit` drop.
        let drained_to_eof = drain_to_child(&mut rx, &mut stdin, &mut child).await;

        // Child is gone (or we observed a clean channel close).
        *pid_slot.lock().await = None;
        drop(stdin);
        let _ = child.start_kill();
        let _ = child.wait().await;

        if drained_to_eof {
            // The channel closed (all senders dropped) — nothing left to do.
            return;
        }

        // Child died with the channel still open: respawn after backoff.
        tracing::warn!("telemetry: child exited; respawning");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
    }
}

/// Move events from the channel to the child's stdin. Returns `true` if the
/// channel closed (clean shutdown), `false` if the child died (needs respawn).
async fn drain_to_child(
    rx: &mut mpsc::Receiver<TelemetryEvent>,
    stdin: &mut tokio::process::ChildStdin,
    child: &mut tokio::process::Child,
) -> bool {
    loop {
        tokio::select! {
            // The child exited — bail to the respawn path.
            status = child.wait() => {
                let _ = status;
                return false;
            }
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => {
                        let mut buf = Vec::new();
                        if ipc::write_frame(&mut buf, &ev).is_err() {
                            continue;
                        }
                        if stdin.write_all(&buf).await.is_err() {
                            // Pipe broke (child died): respawn.
                            return false;
                        }
                        let _ = stdin.flush().await;
                    }
                    // All senders dropped: clean shutdown.
                    None => return true,
                }
            }
        }
    }
}
