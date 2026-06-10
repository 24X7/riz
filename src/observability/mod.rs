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
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use ipc::TelemetryEvent;

/// Min/max backoff between child respawn attempts.
const RESPAWN_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Graceful-shutdown drain budget: how long we keep pulling already-enqueued
/// events out of the channel and into the child before closing its stdin.
const SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
/// How long we wait for the child to flush its batch and exit after we close
/// its stdin (it sees EOF), before force-killing it.
const SHUTDOWN_CHILD_WAIT: Duration = Duration::from_secs(3);

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
    /// The drain+supervise task. Signalled (not aborted) on graceful shutdown so
    /// it can flush; only joined after the signal.
    task: tokio::task::JoinHandle<()>,
    /// Fires on `shutdown()`: tells the supervise loop to stop respawning,
    /// drain the channel, close the child's stdin, and wait for it to flush+exit.
    shutdown_tx: Option<oneshot::Sender<()>>,
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
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let sink = sink.to_path_buf();
        let pid_slot = child_pid.clone();
        let task = tokio::spawn(async move {
            supervise_loop(rx, sink, target, pid_slot, shutdown_rx).await;
        });

        Ok(Self {
            handle,
            child_pid,
            task,
            shutdown_tx: Some(shutdown_tx),
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

    /// Gracefully stop the supervisor and its child WITHOUT losing spans.
    ///
    /// Guarantee: every event that was successfully `emit`'d (enqueued, not
    /// overflow-dropped) before this call is written to the child's sink/export
    /// before this returns, within a bounded timeout.
    ///
    /// Sequence (executed by the supervise loop on the shutdown signal):
    ///   1. Stop respawning the child.
    ///   2. Drain everything currently in the channel into the child's stdin,
    ///      bounded by [`SHUTDOWN_DRAIN_DEADLINE`]. We drain to *empty* (not to
    ///      channel-closed): the `AppState.telemetry` handle clone may still be
    ///      alive, so the channel never closes — waiting for `None` would
    ///      deadlock.
    ///   3. Close the child's stdin (drop the writer) so it sees EOF and flushes
    ///      its buffered batch.
    ///   4. Wait up to [`SHUTDOWN_CHILD_WAIT`] for the child to exit; if it
    ///      overruns, kill it.
    pub async fn shutdown(mut self) {
        // Signal the loop to begin graceful drain. If the loop already exited
        // (channel closed), the receiver is gone and the send is a no-op.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // The loop owns the child's stdin and the drain/close/wait sequence; we
        // just wait for it to finish (bounded internally).
        let _ = self.task.await;
    }
}

/// Why the per-child drain loop returned.
enum DrainExit {
    /// All senders dropped (channel closed): clean, no graceful signal needed.
    ChannelClosed,
    /// The child died with the channel still open: respawn it.
    ChildDied,
    /// `shutdown()` was signalled: perform the bounded graceful flush.
    ShutdownRequested,
}

/// The drain + respawn loop. Runs until the channel is closed (all handles
/// dropped), the child needs respawning, or `shutdown()` is signalled.
async fn supervise_loop(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    sink: PathBuf,
    target: ExportTarget,
    pid_slot: Arc<Mutex<Option<u32>>>,
    mut shutdown_rx: oneshot::Receiver<()>,
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
                // A shutdown while we're failing to spawn should still terminate
                // the loop (nothing to flush to — the child never came up).
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
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

        // Drain the channel into the child's stdin until the child dies, the
        // channel closes, or shutdown is signalled. The channel is the host's
        // only coupling to the child; a stalled child just lets the bounded
        // channel fill and `emit` drop.
        let exit = drain_to_child(&mut rx, &mut stdin, &mut child, &mut shutdown_rx).await;

        match exit {
            DrainExit::ShutdownRequested => {
                // Graceful flush: drain already-enqueued events (bounded), then
                // close stdin so the child flushes its batch on EOF, then wait
                // (bounded) for it to exit; kill it if it overruns.
                graceful_flush(&mut rx, stdin, &mut child).await;
                *pid_slot.lock().await = None;
                return;
            }
            DrainExit::ChannelClosed => {
                // All senders dropped: nothing left to enqueue. Close stdin so
                // the child flushes on EOF, then wait (bounded) for it to exit.
                graceful_flush(&mut rx, stdin, &mut child).await;
                *pid_slot.lock().await = None;
                return;
            }
            DrainExit::ChildDied => {
                *pid_slot.lock().await = None;
                drop(stdin);
                let _ = child.start_kill();
                let _ = child.wait().await;
                // Respawn after backoff — but a shutdown during the backoff ends
                // the loop instead.
                tracing::warn!("telemetry: child exited; respawning");
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
            }
        }
    }
}

/// Move events from the channel to the child's stdin until the child dies, the
/// channel closes, or shutdown is signalled.
async fn drain_to_child(
    rx: &mut mpsc::Receiver<TelemetryEvent>,
    stdin: &mut tokio::process::ChildStdin,
    child: &mut tokio::process::Child,
    shutdown_rx: &mut oneshot::Receiver<()>,
) -> DrainExit {
    loop {
        tokio::select! {
            // The child exited — bail to the respawn path.
            status = child.wait() => {
                let _ = status;
                return DrainExit::ChildDied;
            }
            // Graceful shutdown requested.
            _ = &mut *shutdown_rx => {
                return DrainExit::ShutdownRequested;
            }
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => {
                        if write_event(stdin, &ev).await.is_err() {
                            // Pipe broke (child died): respawn.
                            return DrainExit::ChildDied;
                        }
                    }
                    // All senders dropped: clean shutdown.
                    None => return DrainExit::ChannelClosed,
                }
            }
        }
    }
}

/// Bounded graceful flush, run after the supervise loop decides to stop:
/// drain every event already in the channel (to *empty*, bounded by
/// [`SHUTDOWN_DRAIN_DEADLINE`]; we never wait for channel-close because a
/// surviving handle clone keeps it open), then close stdin so the child sees
/// EOF and flushes its batch, then wait up to [`SHUTDOWN_CHILD_WAIT`] for it to
/// exit, killing it on overrun.
async fn graceful_flush(
    rx: &mut mpsc::Receiver<TelemetryEvent>,
    mut stdin: tokio::process::ChildStdin,
    child: &mut tokio::process::Child,
) {
    // 1. Drain whatever is already enqueued, bounded by a deadline.
    let drain = async {
        // `try_recv` pulls only events already buffered; we loop until the
        // channel is momentarily empty. This delivers every event that was
        // enqueued before shutdown without blocking on never-closing senders.
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    if write_event(&mut stdin, &ev).await.is_err() {
                        // Child died mid-drain; stop draining.
                        return;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => return,
            }
        }
    };
    let _ = tokio::time::timeout(SHUTDOWN_DRAIN_DEADLINE, drain).await;

    // 2. Close the child's stdin so it sees EOF and flushes its batch.
    drop(stdin);

    // 3. Bounded wait for the child to flush + exit; kill it if it overruns.
    match tokio::time::timeout(SHUTDOWN_CHILD_WAIT, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            tracing::warn!("telemetry: child did not exit within shutdown deadline; killing");
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// Frame + write a single event to the child's stdin and flush. A frame-encode
/// failure is skipped (returns Ok); a pipe error returns Err (child gone).
async fn write_event(
    stdin: &mut tokio::process::ChildStdin,
    ev: &TelemetryEvent,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    if ipc::write_frame(&mut buf, ev).is_err() {
        return Ok(());
    }
    stdin.write_all(&buf).await?;
    stdin.flush().await?;
    Ok(())
}
