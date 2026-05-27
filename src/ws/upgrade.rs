//! WebSocket upgrade handler. Accepts the HTTP upgrade, dispatches a
//! `$connect` event to the function, and on `statusCode: 200` registers
//! the connection and spawns the per-connection reader + writer tasks.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
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
use crate::ws::connection::{Connection, ConnectionId, OutboundMessage};
use crate::ws::event::{build_connect, build_disconnect, build_message};

/// axum handler that gets mounted at the WebSocket function's path.
/// Captures the function name in the wrapper closure (see main.rs).
pub async fn ws_upgrade_handler(
    State((state, function_name)): State<(Arc<AppState>, String)>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
) -> Response {
    let stage = state.config.read().await.server.stage.clone();
    let query: HashMap<String, String> = HashMap::new(); // TODO: pull from URI

    ws.on_upgrade(move |socket| async move {
        handle_socket(state, function_name, stage, headers, query, socket).await;
    })
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

    // 1. Dispatch $connect. If non-200, close immediately.
    let connect_evt = build_connect(
        &stage,
        connection_id.as_str(),
        connected_at_ms,
        // The upgrade path — fetch from the function's first declared route.
        "/", // overwritten just below
        headers.clone(),
        query.clone(),
    );

    let connect_resp: ApiGatewayProxyResponse = match state
        .process_manager
        .invoke_generic(&function_name, &connect_evt, timeout_ms)
        .await
    {
        Ok(r) => r,
        Err(PoolError::Timeout(ref name, ms)) => {
            warn!(function = %name, timeout_ms = ms, "ws $connect timed out — closing connection");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        Err(PoolError::InvalidResponse(ref name, ref detail)) => {
            warn!(function = %name, detail = %detail, "ws $connect malformed response — closing connection");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        Err(PoolError::SemaphoreExhausted(ref name)) => {
            warn!(function = %name, "ws $connect semaphore exhausted — closing connection");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        Err(e) => {
            warn!("ws $connect failed for {function_name}: {e}");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    if connect_resp.status_code != 200 {
        warn!(
            "ws $connect rejected by {function_name}: status {}",
            connect_resp.status_code
        );
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    // 2. Register connection.
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<OutboundMessage>();
    // Keep a local sender clone so the teardown path can queue a Close frame
    // and then drop both senders, terminating the writer's recv() loop naturally.
    let outbound_local = outbound_tx.clone();
    let (close_tx, mut close_rx) = oneshot::channel::<()>();
    let conn = Arc::new(Connection {
        id: connection_id.clone(),
        function_name: function_name.clone(),
        connected_at,
        last_active: std::sync::Mutex::new(connected_at),
        outbound: outbound_tx,
        close_signal: std::sync::Mutex::new(Some(close_tx)),
    });
    if let Err(reason) = state.ws_connections.try_insert(conn.clone()) {
        warn!(
            function = %function_name,
            connection_id = %connection_id,
            "ws connection rejected: {reason} (RIZ_MAX_CONNECTIONS ceiling)"
        );
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    info!(
        "ws connected: {} (function {})",
        connection_id, function_name
    );

    // 3. Split the socket. Writer task reads from outbound_rx, sends to client.
    //    Reader loop in this task: each Message → dispatch $default event.
    let (mut sink, mut stream) = futures_util::StreamExt::split(socket);
    use futures_util::SinkExt;

    let writer = tokio::spawn(async move {
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
    });

    // Reader loop — terminates on client disconnect, server close signal,
    // or stream error. Either way we dispatch $disconnect on the way out.
    let read_state = state.clone();
    let read_fn = function_name.clone();
    let read_id = connection_id.clone();
    loop {
        tokio::select! {
            biased;
            _ = &mut close_rx => break,
            msg = futures_util::StreamExt::next(&mut stream) => {
                let Some(msg) = msg else { break };
                let Ok(msg) = msg else { break };
                conn.touch();
                match msg {
                    Message::Text(text) => {
                        let ev = build_message(&stage, read_id.as_str(), connected_at_ms, Some(text), false);
                        match read_state.process_manager
                            .invoke_generic::<_, ApiGatewayProxyResponse>(&read_fn, &ev, timeout_ms)
                            .await
                        {
                            Ok(_) => {}
                            Err(PoolError::Timeout(ref name, ms)) => {
                                warn!(function = %name, timeout_ms = ms, "ws $default timed out — closing connection");
                                break;
                            }
                            Err(PoolError::InvalidResponse(ref name, ref detail)) => {
                                warn!(function = %name, detail = %detail, "ws $default malformed response — closing connection");
                                break;
                            }
                            Err(PoolError::SemaphoreExhausted(ref name)) => {
                                warn!(function = %name, "ws $default semaphore exhausted (transient backpressure)");
                                // keep connection open — transient backpressure
                            }
                            Err(e) => {
                                warn!("ws $default dispatch error on {read_fn}: {e}");
                            }
                        }
                    }
                    Message::Binary(bytes) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let ev = build_message(&stage, read_id.as_str(), connected_at_ms, Some(b64), true);
                        match read_state.process_manager
                            .invoke_generic::<_, ApiGatewayProxyResponse>(&read_fn, &ev, timeout_ms)
                            .await
                        {
                            Ok(_) => {}
                            Err(PoolError::Timeout(ref name, ms)) => {
                                warn!(function = %name, timeout_ms = ms, "ws $default (binary) timed out — closing connection");
                                break;
                            }
                            Err(PoolError::InvalidResponse(ref name, ref detail)) => {
                                warn!(function = %name, detail = %detail, "ws $default (binary) malformed response — closing connection");
                                break;
                            }
                            Err(PoolError::SemaphoreExhausted(ref name)) => {
                                warn!(function = %name, "ws $default (binary) semaphore exhausted (transient backpressure)");
                                // keep connection open — transient backpressure
                            }
                            Err(e) => {
                                warn!("ws $default (binary) dispatch error on {read_fn}: {e}");
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {} // axum auto-pongs
                }
            }
        }
    }

    // 4. Dispatch $disconnect (best-effort), remove from store, wait for writer.
    let disc_evt = build_disconnect(&stage, read_id.as_str(), connected_at_ms);
    let _ = state
        .process_manager
        .invoke_generic::<_, ApiGatewayProxyResponse>(&function_name, &disc_evt, timeout_ms)
        .await;

    // Remove from store — this drops the Arc<Connection> held by the store,
    // but `conn` (this task) and `outbound_local` still hold sender references.
    state.ws_connections.remove(&read_id);

    // Queue a Close frame so the writer sends a clean WebSocket close to the
    // client (idempotent: if management API already queued one, it drains first).
    let _ = outbound_local.send(OutboundMessage::Close);

    // Drop both remaining sender handles (the local clone and the conn Arc) so
    // the writer's unbounded_channel recv() returns None after draining the queue.
    drop(outbound_local);
    drop(conn);

    // Wait up to 5 s for the writer to flush queued messages and exit cleanly.
    // Fall back to abort() only on timeout — clients see CLOSE instead of RST.
    let writer_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    tokio::pin!(writer);
    tokio::select! {
        _ = &mut writer => {}
        _ = tokio::time::sleep_until(writer_deadline) => {
            warn!("ws writer task timed out for {} — aborting", read_id);
            writer.abort();
        }
    }

    info!("ws disconnected: {} (function {})", read_id, function_name);
}
