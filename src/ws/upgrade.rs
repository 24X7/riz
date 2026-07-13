//! WebSocket upgrade handler. Accepts the HTTP upgrade, dispatches a
//! `$connect` event to the function, and on `statusCode: 200` registers
//! the connection and spawns the per-connection reader + writer tasks.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, RawQuery, State};
use axum::response::Response;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;
use tracing::{info, warn};

use crate::gateway::ApiGatewayProxyResponse;
use crate::process::PoolError;
use crate::state::AppState;
use crate::ws::connection::{Connection, ConnectionId, OutboundMessage, OUTBOUND_CAPACITY};
use crate::ws::event::{build_connect, build_disconnect, build_message};

/// Inbound size caps for client → server WebSocket traffic (Power of 10
/// rule 3: every buffer growing from remote input carries an explicit cap).
/// The values are AWS API Gateway's WebSocket quotas — riz's wire-parity
/// target — so an app tested on riz cannot come to depend on frames that AWS
/// would reject. Enforced by the transport: an oversized frame errors the
/// read stream, which tears the connection down through the normal
/// `$disconnect` path.
const MAX_INBOUND_FRAME_BYTES: usize = 32 * 1024; // AWS quota: 32 KB/frame
const MAX_INBOUND_MESSAGE_BYTES: usize = 128 * 1024; // AWS quota: 128 KB/message

/// How long teardown waits for the writer task to flush queued frames before
/// aborting it (clients then see a TCP close instead of a WS CLOSE).
const WRITER_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// axum handler that gets mounted at the WebSocket function's path.
/// Captures the function name in the wrapper closure (see main.rs).
pub async fn ws_upgrade_handler(
    State((state, function_name)): State<(Arc<AppState>, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    // Per-caller API-key gate — a WebSocket function is a data-plane surface too,
    // so the handshake must pass the same `[api_keys]` admission as an HTTP
    // invocation (no keys configured → open). Reject before upgrading; the
    // client sees the 401/429 as the handshake response.
    if let Some(resp) = crate::server::api_key_admission(
        &state,
        &headers,
        &peer.ip().to_string(),
        &format!("WS {function_name}"),
    )
    .await
    {
        return resp;
    }

    let stage = state.config.read().await.server.stage.clone();
    // Parse queryStringParameters from the upgrade request URI so $connect
    // events carry them, matching the AWS WebSocket event shape + the HTTP path.
    let query: HashMap<String, String> = raw_query
        .as_deref()
        .map(parse_query_string)
        .unwrap_or_default();

    ws.max_frame_size(MAX_INBOUND_FRAME_BYTES)
        .max_message_size(MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            handle_socket(state, function_name, stage, headers, query, socket).await;
        })
}

/// Everything the `$connect` / `$default` / `$disconnect` dispatch helpers
/// need — one struct so the rule-4 split does not smuggle its complexity
/// into seven-argument parameter lists.
struct WsDispatchCtx {
    state: Arc<AppState>,
    function_name: String,
    stage: String,
    connection_id: ConnectionId,
    connected_at_ms: i64,
    timeout_ms: u64,
}

/// What the read loop should do after handling one inbound message.
enum InboundOutcome {
    KeepOpen,
    Close,
}

async fn handle_socket(
    state: Arc<AppState>,
    function_name: String,
    stage: String,
    headers: axum::http::HeaderMap,
    query: HashMap<String, String>,
    mut socket: WebSocket,
) {
    let connection_id = ConnectionId::new();
    let connected_at = Instant::now();
    let connected_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Record per-connection trace context. We use Span::current() to attach
    // fields to the ambient span rather than entering a new one across awaits
    // (EnteredSpan is !Send and cannot be held across .await points).
    tracing::Span::current().record("ws_connection_id", connection_id.as_str());
    tracing::Span::current().record("ws_function", function_name.as_str());

    // Look up the function config to get timeout_ms.
    let timeout_ms = {
        let cfg = state.config.read().await;
        cfg.functions
            .get(&function_name)
            .map(|f| f.timeout_ms)
            .unwrap_or(30_000)
    };

    let ctx = WsDispatchCtx {
        state: state.clone(),
        function_name,
        stage,
        connection_id: connection_id.clone(),
        connected_at_ms,
        timeout_ms,
    };

    // 1. Dispatch $connect. On any dispatch failure or non-200, close
    //    immediately — the connection is never registered.
    if !dispatch_connect(&ctx, headers, query, &mut socket).await {
        return;
    }

    // 2. Register connection. Bounded queue (rule 3): the writer drains at
    //    socket speed; a slow client filling it surfaces as try_send errors
    //    to pushers instead of unbounded buffering.
    let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundMessage>(OUTBOUND_CAPACITY);
    // Keep a local sender clone so the teardown path can queue a Close frame
    // and then drop both senders, terminating the writer's recv() loop naturally.
    let outbound_local = outbound_tx.clone();
    let (close_tx, close_rx) = oneshot::channel::<()>();
    let conn = Arc::new(Connection {
        id: connection_id.clone(),
        function_name: ctx.function_name.clone(),
        connected_at,
        last_active: std::sync::Mutex::new(connected_at),
        outbound: outbound_tx,
        close_signal: std::sync::Mutex::new(Some(close_tx)),
    });
    if let Err(reason) = state.ws_connections.try_insert(conn.clone()) {
        warn!(
            function = %ctx.function_name,
            connection_id = %connection_id,
            "ws connection rejected: {reason} (RIZ_MAX_CONNECTIONS ceiling)"
        );
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    info!(
        "ws connected: {} (function {})",
        connection_id, ctx.function_name
    );

    // 3. Split the socket. Writer task reads from outbound_rx, sends to the
    //    client; the reader loop dispatches each Message as a $default event.
    let (sink, stream) = futures_util::StreamExt::split(socket);
    let writer = spawn_writer(sink, outbound_rx);
    read_loop(&ctx, &conn, stream, close_rx).await;

    // 4. Dispatch $disconnect (best-effort), remove from store, wait for writer.
    teardown(&ctx, conn, outbound_local, writer).await;
}

/// Step 1: dispatch `$connect`. Returns `true` when the function answered 200
/// and the connection may proceed; on every dispatch failure or non-200
/// status it sends a Close frame and returns `false`.
async fn dispatch_connect(
    ctx: &WsDispatchCtx,
    headers: axum::http::HeaderMap,
    query: HashMap<String, String>,
    socket: &mut WebSocket,
) -> bool {
    let connect_evt = build_connect(
        &ctx.stage,
        ctx.connection_id.as_str(),
        ctx.connected_at_ms,
        // The mount path is not threaded through to $connect today; AWS
        // parity for requestContext identity fields is tracked separately.
        "/",
        headers,
        query,
    );

    let connect_start = std::time::Instant::now();
    let connect_resp: ApiGatewayProxyResponse = match ctx
        .state
        .process_manager
        .invoke_generic(&ctx.function_name, &connect_evt, ctx.timeout_ms)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Every dispatch failure closes: PoolError's Display carries the
            // variant detail (timeout ms, malformed-response detail,
            // semaphore exhaustion), so one arm loses no diagnostics.
            warn!(function = %ctx.function_name, "ws $connect failed — closing connection: {e}");
            let _ = socket.send(Message::Close(None)).await;
            return false;
        }
    };
    let connect_latency = connect_start.elapsed().as_secs_f64() * 1000.0;
    let connect_healthy = connect_resp.status_code < 500;
    ctx.state
        .riz_state
        .record_invocation(&ctx.function_name, connect_latency, connect_healthy, false)
        .await;
    ctx.state.push_log(
        "INFO",
        Some(&ctx.function_name),
        format!(
            "WS $connect {} {:.0}ms conn={} fn={}",
            connect_resp.status_code, connect_latency, ctx.connection_id, ctx.function_name
        ),
    );
    if connect_resp.status_code != 200 {
        warn!(
            "ws $connect rejected by {}: status {}",
            ctx.function_name, connect_resp.status_code
        );
        let _ = socket.send(Message::Close(None)).await;
        return false;
    }
    true
}

/// Writer task: drains the bounded outbound queue to the client at socket
/// speed. Exits on a queued Close frame, a send failure, or when every
/// sender handle is dropped (recv → None after the drain).
fn spawn_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut outbound_rx: mpsc::Receiver<OutboundMessage>,
) -> tokio::task::JoinHandle<()> {
    use futures_util::SinkExt;
    tokio::spawn(async move {
        while let Some(msg) = outbound_rx.recv().await {
            let frame = match msg {
                OutboundMessage::Text(s) => Message::Text(s),
                OutboundMessage::Binary(b) => Message::Binary(b),
                OutboundMessage::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            };
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    })
}

/// Step 3 reader loop — terminates on client disconnect, the server close
/// signal, a stream error (including a frame over the inbound size caps), or
/// a dispatch outcome that closes the connection. Supervised event-loop
/// contract (rule 2): awaits every iteration, bounded work per iteration
/// (one frame → at most one function invocation), exits on the close signal.
async fn read_loop(
    ctx: &WsDispatchCtx,
    conn: &Connection,
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    mut close_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut close_rx => break,
            msg = futures_util::StreamExt::next(&mut stream) => {
                let Some(Ok(msg)) = msg else { break };
                conn.touch();
                let outcome = match msg {
                    Message::Text(text) => {
                        let msg_bytes = text.len();
                        dispatch_inbound(ctx, text, false, msg_bytes).await
                    }
                    Message::Binary(bytes) => {
                        use base64::Engine;
                        let msg_bytes = bytes.len();
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        dispatch_inbound(ctx, b64, true, msg_bytes).await
                    }
                    Message::Close(_) => InboundOutcome::Close,
                    Message::Ping(_) | Message::Pong(_) => InboundOutcome::KeepOpen, // axum auto-pongs
                };
                if matches!(outcome, InboundOutcome::Close) {
                    break;
                }
            }
        }
    }
}

/// Dispatch one inbound frame as a `$default` event and translate the result
/// into keep-open/close. Text and binary frames get identical handling — the
/// only differences are the log label and the base64 flag on the event.
async fn dispatch_inbound(
    ctx: &WsDispatchCtx,
    payload: String,
    is_binary: bool,
    msg_bytes: usize,
) -> InboundOutcome {
    let label = if is_binary {
        "$default(bin)"
    } else {
        "$default"
    };
    let ev = build_message(
        &ctx.stage,
        ctx.connection_id.as_str(),
        ctx.connected_at_ms,
        Some(payload),
        is_binary,
    );
    let start = std::time::Instant::now();
    let result = ctx
        .state
        .process_manager
        .invoke_generic::<_, ApiGatewayProxyResponse>(&ctx.function_name, &ev, ctx.timeout_ms)
        .await;
    let latency = start.elapsed().as_secs_f64() * 1000.0;
    let conn_id = &ctx.connection_id;
    let fn_name = &ctx.function_name;
    let resp = match result {
        Ok(resp) => resp,
        Err(e) => {
            // One decision table instead of four near-identical arms:
            // (log level, log message, records an invocation, closes).
            // Timeout / malformed close the connection; backpressure keeps
            // it open without recording (the invocation never ran); every
            // other error keeps it open but records the failure.
            let (level, message, records, closes) = match &e {
                PoolError::Timeout(name, ms) => (
                    "WARN",
                    format!("WS {label} timeout {ms}ms conn={conn_id} fn={name}"),
                    true,
                    true,
                ),
                PoolError::InvalidResponse(name, detail) => (
                    "ERROR",
                    format!("WS {label} malformed conn={conn_id} fn={name}: {detail}"),
                    true,
                    true,
                ),
                PoolError::SemaphoreExhausted(name) => (
                    "WARN",
                    format!("WS {label} backpressure conn={conn_id} fn={name}"),
                    false,
                    false,
                ),
                _ => (
                    "ERROR",
                    format!("WS {label} error conn={conn_id} fn={fn_name}: {e}"),
                    true,
                    false,
                ),
            };
            if records {
                ctx.state
                    .riz_state
                    .record_invocation(fn_name, latency, false, false)
                    .await;
            }
            warn!(function = %fn_name, closes, "ws {label} dispatch failed: {e}");
            ctx.state.push_log(level, Some(fn_name), message);
            return if closes {
                InboundOutcome::Close
            } else {
                InboundOutcome::KeepOpen
            };
        }
    };
    let healthy = resp.status_code < 500;
    ctx.state
        .riz_state
        .record_invocation(fn_name, latency, healthy, false)
        .await;
    ctx.state.push_log(
        "INFO",
        Some(fn_name),
        format!(
            "WS {label} {} {latency:.0}ms conn={conn_id} bytes={msg_bytes} fn={fn_name}",
            resp.status_code
        ),
    );
    InboundOutcome::KeepOpen
}

/// Step 4: dispatch `$disconnect` (best-effort), remove the connection from
/// the store, queue a final Close frame, and give the writer a bounded window
/// to flush before aborting it.
async fn teardown(
    ctx: &WsDispatchCtx,
    conn: Arc<Connection>,
    outbound_local: mpsc::Sender<OutboundMessage>,
    writer: tokio::task::JoinHandle<()>,
) {
    let disc_evt = build_disconnect(&ctx.stage, ctx.connection_id.as_str(), ctx.connected_at_ms);
    let disc_start = std::time::Instant::now();
    let disc_result = ctx
        .state
        .process_manager
        .invoke_generic::<_, ApiGatewayProxyResponse>(&ctx.function_name, &disc_evt, ctx.timeout_ms)
        .await;
    let disc_latency = disc_start.elapsed().as_secs_f64() * 1000.0;
    let (disc_status, disc_healthy) = match &disc_result {
        Ok(r) => (r.status_code, r.status_code < 500),
        Err(_) => (0i64, false),
    };
    ctx.state
        .riz_state
        .record_invocation(&ctx.function_name, disc_latency, disc_healthy, false)
        .await;
    ctx.state.push_log(
        "INFO",
        Some(&ctx.function_name),
        format!(
            "WS $disconnect {disc_status} {disc_latency:.0}ms conn={} fn={}",
            ctx.connection_id, ctx.function_name
        ),
    );

    // Remove from store — this drops the Arc<Connection> held by the store,
    // but `conn` (this task) and `outbound_local` still hold sender references.
    ctx.state.ws_connections.remove(&ctx.connection_id);

    // Queue a Close frame so the writer sends a clean WebSocket close to the
    // client (idempotent: if management API already queued one, it drains first).
    // try_send: if the bounded queue is still full of unflushed frames, skip
    // it — the sender drops below end the writer loop after the drain, and the
    // client sees a TCP close instead of a WS CLOSE (degraded but bounded;
    // never block teardown on a slow client).
    let _ = outbound_local.try_send(OutboundMessage::Close);

    // Drop both remaining sender handles (the local clone and the conn Arc) so
    // the writer's channel recv() returns None after draining the queue.
    drop(outbound_local);
    drop(conn);

    // Wait up to WRITER_FLUSH_TIMEOUT for the writer to flush queued messages
    // and exit cleanly. Fall back to abort() only on timeout — clients see
    // CLOSE instead of RST.
    tokio::pin!(writer);
    tokio::select! {
        _ = &mut writer => {}
        _ = tokio::time::sleep(WRITER_FLUSH_TIMEOUT) => {
            warn!("ws writer task timed out for {} — aborting", ctx.connection_id);
            writer.abort();
        }
    }

    info!(
        "ws disconnected: {} (function {})",
        ctx.connection_id, ctx.function_name
    );
}

/// Parse a raw query string ("a=1&b=hello%20world&flag") into single-value,
/// percent-decoded params. Mirrors the HTTP path's parsing (src/server.rs) so
/// WebSocket `$connect` events carry queryStringParameters the same way.
fn parse_query_string(raw: &str) -> HashMap<String, String> {
    let mut acc = HashMap::new();
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            acc.insert(
                crate::router::percent_decode(k),
                crate::router::percent_decode(v),
            );
        } else {
            acc.insert(crate::router::percent_decode(pair), String::new());
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::parse_query_string;

    #[test]
    fn parse_query_string_decodes_pairs() {
        let q = parse_query_string("a=1&b=hello%20world&flag");
        assert_eq!(q.get("a").map(String::as_str), Some("1"));
        assert_eq!(q.get("b").map(String::as_str), Some("hello world"));
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
    }

    #[test]
    fn parse_query_string_empty_is_empty() {
        assert!(parse_query_string("").is_empty());
    }
}
