//! The daemon-side broker service — one supervised UDS accept loop that owns
//! the capability blast radius for the whole process.
//!
//! Topology: the broker lives HERE (in the riz daemon), not in each
//! `__wasm-host` child. A granted worker connects to `broker.sock`, presents
//! its per-function token, and issues capability calls; the daemon holds the
//! shared connection pools, the per-function limit state, and every
//! credential. Nothing crosses to the child but a socket path and a token.
//!
//! Security, layered:
//! 1. The socket lives in a `0700` directory, so **only the daemon's own uid
//!    can traverse to it** — the kernel VFS enforces the peer-uid check for
//!    us on both Linux and macOS (no platform-specific `SO_PEERCRED`). The
//!    socket file itself is `0600`.
//! 2. Each granted function gets a random 32-byte token, handed only to that
//!    function's workers via env at spawn. A connection must present a valid
//!    token in its HELLO frame or it is dropped — a token authorizes exactly
//!    one function's grant table, so a compromised worker can never exercise
//!    another function's grants.
//! 3. Grantless functions get no token and no socket env — their host import
//!    answers `denied` locally, with zero IPC.
//!
//! The accept loop is a supervised event loop per docs/SAFETY.md rule 2:
//! top-level of a spawned task, `await`s every iteration, does bounded work
//! (one accept, then hands off), and exits on the shutdown signal.

use super::wire::{CallPayload, Frame, FrameType};
use super::Broker;
use crate::config::Config;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, Semaphore};

/// A per-function random token, rendered as lowercase hex for env transport.
type TokenHex = String;

/// State shared between the accept loop and the spawn-time [`BrokerHandle`].
struct BrokerShared {
    sock_path: PathBuf,
    /// token → function name. Presented in HELLO; maps to the grant table.
    tokens: HashMap<TokenHex, String>,
    /// function name → its armed [`Broker`] (grant limits + shared pools).
    brokers: HashMap<String, Arc<Broker>>,
    /// function name → the token minted for it (for [`BrokerHandle::env_for`]).
    function_token: HashMap<String, TokenHex>,
    /// The max grant `call_timeout_ms` across all grants — the child arms its
    /// socket timeout at this + slack, so a wedged daemon never hangs a guest.
    max_call_timeout_ms: u64,
}

/// A cheap-clone handle used at worker-spawn time to hand a granted wasm
/// worker its broker env. Held by `ProcessManager`.
#[derive(Clone)]
pub struct BrokerHandle {
    shared: Arc<BrokerShared>,
}

impl BrokerHandle {
    /// The env vars a granted worker of `function` needs to reach the broker:
    /// `RIZ_BROKER_SOCK`, `RIZ_BROKER_TOKEN`, `RIZ_BROKER_TIMEOUT_MS`. Empty
    /// for a function with no grants (it gets no socket → local `denied`).
    pub fn env_for(&self, function: &str) -> Vec<(String, String)> {
        let Some(token) = self.shared.function_token.get(function) else {
            return Vec::new();
        };
        vec![
            (
                "RIZ_BROKER_SOCK".to_string(),
                self.shared.sock_path.to_string_lossy().into_owned(),
            ),
            ("RIZ_BROKER_TOKEN".to_string(), token.clone()),
            (
                "RIZ_BROKER_TIMEOUT_MS".to_string(),
                self.shared
                    .max_call_timeout_ms
                    .saturating_add(250)
                    .to_string(),
            ),
        ]
    }
}

/// The running broker service. Dropping it (or calling [`Self::shutdown`])
/// stops the accept loop and removes the socket directory.
pub struct BrokerService {
    handle: BrokerHandle,
    task: tokio::task::JoinHandle<()>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// The 0700 socket dir; removed on drop.
    _dir: tempfile::TempDir,
}

impl BrokerService {
    /// Start the broker if any function declares capabilities. Resolves every
    /// referenced resource's DSN NOW (daemon-startup fail-fast) and arms one
    /// [`Broker`] per granted function over shared pools. Returns `Ok(None)`
    /// when no function has grants — no socket, no task, no cost.
    pub async fn start(config: &Config) -> anyhow::Result<Option<BrokerService>> {
        let granted: Vec<(&String, &crate::config::FunctionConfig)> = config
            .functions
            .iter()
            .filter(|(_, f)| !f.capabilities.is_empty())
            .collect();
        if granted.is_empty() {
            return Ok(None);
        }

        // One shared pool per pg resource, built once and reused by every
        // function's Broker — the connection cap is on the backend.
        let mut brokers: HashMap<String, Arc<Broker>> = HashMap::new();
        let mut tokens: HashMap<TokenHex, String> = HashMap::new();
        let mut function_token: HashMap<String, TokenHex> = HashMap::new();
        let mut max_call_timeout_ms = 0u64;
        for (name, func) in &granted {
            let backends = super::backends_for_function(&func.capabilities, &config.resources)
                .map_err(|e| anyhow::anyhow!("broker setup for function '{name}': {e}"))?;
            brokers.insert(
                (*name).clone(),
                Arc::new(Broker::from_backends(&func.capabilities, backends)),
            );
            let token = mint_token_hex()?;
            tokens.insert(token.clone(), (*name).clone());
            function_token.insert((*name).clone(), token);
            for grant in func.capabilities.values() {
                max_call_timeout_ms = max_call_timeout_ms.max(grant.call_timeout_ms);
            }
        }

        // 0700 dir + 0600 socket: only our uid can traverse to the socket.
        let dir = tempfile::Builder::new().prefix("riz-broker-").tempdir()?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let sock_path = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&sock_path)?;
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;

        // Bound concurrent connections at 2× the total granted concurrency —
        // enough for warm workers plus a respawn overlap, never unbounded.
        let conn_budget: usize = granted
            .iter()
            .map(|(_, f)| f.concurrency.saturating_mul(2).max(2))
            .sum();

        let shared = Arc::new(BrokerShared {
            sock_path: sock_path.clone(),
            tokens,
            brokers,
            function_token,
            max_call_timeout_ms,
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let accept_shared = shared.clone();
        let task = tokio::spawn(async move {
            accept_loop(listener, accept_shared, conn_budget, shutdown_rx).await;
        });

        tracing::info!(
            target: "riz::broker",
            functions = shared.brokers.len(),
            sock = %sock_path.display(),
            "broker service started"
        );
        Ok(Some(BrokerService {
            handle: BrokerHandle { shared },
            task,
            shutdown_tx: Some(shutdown_tx),
            _dir: dir,
        }))
    }

    /// The spawn-time handle for `ProcessManager` to mint worker env.
    pub fn handle(&self) -> BrokerHandle {
        self.handle.clone()
    }

    /// Stop accepting, drain the accept task, and remove the socket dir.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
        // `_dir` (TempDir) unlinks the socket + dir on drop here.
    }
}

/// Mint a 32-byte random token as lowercase hex.
fn mint_token_hex() -> anyhow::Result<TokenHex> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("token rng failed: {e}"))?;
    let mut s = String::with_capacity(64);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

/// The supervised accept loop (SAFETY rule 2): await every iteration, bounded
/// work per iteration, exit on shutdown.
async fn accept_loop(
    listener: UnixListener,
    shared: Arc<BrokerShared>,
    conn_budget: usize,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let sem = Arc::new(Semaphore::new(conn_budget));
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::debug!(target: "riz::broker", "broker accept loop shutting down");
                return;
            }
            accepted = listener.accept() => {
                admit_connection(accepted, &shared, &sem);
            }
        }
    }
}

/// One accept: on success and a free connection slot, hand off to a per-conn
/// task; otherwise drop (reject-not-queue). Bounded work, no `.await`.
fn admit_connection(
    accepted: std::io::Result<(UnixStream, tokio::net::unix::SocketAddr)>,
    shared: &Arc<BrokerShared>,
    sem: &Arc<Semaphore>,
) {
    let stream = match accepted {
        Ok((stream, _addr)) => stream,
        Err(e) => {
            tracing::warn!(target: "riz::broker", "accept failed: {e}");
            return;
        }
    };
    // try_acquire: at the connection cap we drop the newcomer rather than
    // queue it — reject-not-queue, per SAFETY rule 3.
    let Ok(permit) = sem.clone().try_acquire_owned() else {
        tracing::warn!(target: "riz::broker", "connection budget exhausted; dropping peer");
        return;
    };
    let conn_shared = shared.clone();
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(e) = serve_connection(stream, conn_shared).await {
            tracing::debug!(target: "riz::broker", "connection ended: {e}");
        }
    });
}

/// One connection: HELLO auth (2s deadline), then a CALL→REPLY loop until the
/// peer hangs up. A protocol error drops the connection; the guest's client
/// answers the guest with a `backend`/`timeout` envelope, so a dropped
/// connection is never a hung guest.
async fn serve_connection(mut stream: UnixStream, shared: Arc<BrokerShared>) -> anyhow::Result<()> {
    // Authenticate within a bounded window so a silent peer can't pin a slot.
    let hello = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut stream))
        .await
        .map_err(|_| anyhow::anyhow!("HELLO timed out"))??;
    if hello.frame_type != FrameType::Hello {
        anyhow::bail!("first frame was not HELLO");
    }
    let token = String::from_utf8_lossy(&hello.payload).into_owned();
    let Some(function) = shared.tokens.get(&token) else {
        anyhow::bail!("bad token");
    };
    let Some(broker) = shared.brokers.get(function) else {
        anyhow::bail!("no broker for function '{function}'");
    };

    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(f) => f,
            // EOF / reset → the worker went away; a clean end, not an error.
            Err(_) => return Ok(()),
        };
        if frame.frame_type != FrameType::Call {
            anyhow::bail!("expected CALL, got {:?}", frame.frame_type);
        }
        let call = CallPayload::decode(&frame.payload)
            .map_err(|e| anyhow::anyhow!("malformed CALL: {e}"))?;
        let response = broker.dispatch(&call.verb, &call.grant, &call.body).await;
        let reply = Frame::new(FrameType::Reply, frame.call_id, response);
        let bytes = reply
            .encode()
            .map_err(|e| anyhow::anyhow!("encode REPLY: {e}"))?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
    }
}

/// Read one length-prefixed BCP frame from the stream, rejecting an oversize
/// declared length before allocating the body.
async fn read_frame(stream: &mut UnixStream) -> anyhow::Result<Frame> {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).await?;
    let body_len = Frame::declared_body_len(prefix).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body).await?;
    Frame::decode_body(&body).map_err(|e| anyhow::anyhow!("{e}"))
}
