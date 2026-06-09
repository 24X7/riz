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
use crate::ws::connection::{Connection, ConnectionId, OutboundMessage};
use crate::ws::event::{build_connect, build_disconnect, build_message};

/// axum handler that gets mounted at the WebSocket function's path.
/// Captures the function name in the wrapper closure (see main.rs).
pub async fn ws_upgrade_handler(
    State((state, function_name)): State<(Arc<AppState>, String)>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let stage = state.config.read().await.server.stage.clone();
    // Parse queryStringParameters from the upgrade request URI so $connect
    // events carry them, matching the AWS WebSocket event shape + the HTTP path.
    let query: HashMap<String, String> =
        raw_query.as_deref().map(parse_query_string).unwrap_or_default();

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

    let connect_start = std::time::Instant::now();
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
    let connect_latency = connect_start.elapsed().as_secs_f64() * 1000.0;
    let connect_healthy = connect_resp.status_code < 500;
    state
        .riz_state
        .record_invocation(&function_name, connect_latency, connect_healthy, false)
        .await;
    state.push_log(
        "INFO",
        Some(&function_name),
        format!(
            "WS $connect {} {:.0}ms conn={} fn={function_name}",
            connect_resp.status_code, connect_latency, connection_id
        ),
    );
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
                        let msg_bytes = text.len();
                        let ev = build_message(&stage, read_id.as_str(), connected_at_ms, Some(text), false);
                        let start = std::time::Instant::now();
                        let result = read_state.process_manager
                            .invoke_generic::<_, ApiGatewayProxyResponse>(&read_fn, &ev, timeout_ms)
                            .await;
                        let latency = start.elapsed().as_secs_f64() * 1000.0;
                        match result {
                            Ok(resp) => {
                                let healthy = resp.status_code < 500;
                                read_state.riz_state
                                    .record_invocation(&read_fn, latency, healthy, false)
                                    .await;
                                read_state.push_log(
                                    "INFO",
                                    Some(&read_fn),
                                    format!(
                                        "WS $default {} {:.0}ms conn={} bytes={msg_bytes} fn={read_fn}",
                                        resp.status_code, latency, read_id
                                    ),
                                );
                            }
                            Err(PoolError::Timeout(ref name, ms)) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("WARN", Some(name), format!("WS $default timeout {ms}ms conn={read_id} fn={name}"));
                                warn!(function = %name, timeout_ms = ms, "ws $default timed out — closing connection");
                                break;
                            }
                            Err(PoolError::InvalidResponse(ref name, ref detail)) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("ERROR", Some(name), format!("WS $default malformed conn={read_id} fn={name}: {detail}"));
                                warn!(function = %name, detail = %detail, "ws $default malformed response — closing connection");
                                break;
                            }
                            Err(PoolError::SemaphoreExhausted(ref name)) => {
                                read_state.push_log("WARN", Some(name), format!("WS $default backpressure conn={read_id} fn={name}"));
                                warn!(function = %name, "ws $default semaphore exhausted (transient backpressure)");
                                // keep connection open — transient backpressure
                            }
                            Err(e) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("ERROR", Some(&read_fn), format!("WS $default error conn={read_id} fn={read_fn}: {e}"));
                                warn!("ws $default dispatch error on {read_fn}: {e}");
                            }
                        }
                    }
                    Message::Binary(bytes) => {
                        use base64::Engine;
                        let msg_bytes = bytes.len();
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let ev = build_message(&stage, read_id.as_str(), connected_at_ms, Some(b64), true);
                        let start = std::time::Instant::now();
                        let result = read_state.process_manager
                            .invoke_generic::<_, ApiGatewayProxyResponse>(&read_fn, &ev, timeout_ms)
                            .await;
                        let latency = start.elapsed().as_secs_f64() * 1000.0;
                        match result {
                            Ok(resp) => {
                                let healthy = resp.status_code < 500;
                                read_state.riz_state
                                    .record_invocation(&read_fn, latency, healthy, false)
                                    .await;
                                read_state.push_log(
                                    "INFO",
                                    Some(&read_fn),
                                    format!(
                                        "WS $default(bin) {} {:.0}ms conn={} bytes={msg_bytes} fn={read_fn}",
                                        resp.status_code, latency, read_id
                                    ),
                                );
                            }
                            Err(PoolError::Timeout(ref name, ms)) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("WARN", Some(name), format!("WS $default(bin) timeout {ms}ms conn={read_id} fn={name}"));
                                warn!(function = %name, timeout_ms = ms, "ws $default (binary) timed out — closing connection");
                                break;
                            }
                            Err(PoolError::InvalidResponse(ref name, ref detail)) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("ERROR", Some(name), format!("WS $default(bin) malformed conn={read_id} fn={name}: {detail}"));
                                warn!(function = %name, detail = %detail, "ws $default (binary) malformed response — closing connection");
                                break;
                            }
                            Err(PoolError::SemaphoreExhausted(ref name)) => {
                                read_state.push_log("WARN", Some(name), format!("WS $default(bin) backpressure conn={read_id} fn={name}"));
                                warn!(function = %name, "ws $default (binary) semaphore exhausted (transient backpressure)");
                            }
                            Err(e) => {
                                read_state.riz_state.record_invocation(&read_fn, latency, false, false).await;
                                read_state.push_log("ERROR", Some(&read_fn), format!("WS $default(bin) error conn={read_id} fn={read_fn}: {e}"));
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
    let disc_start = std::time::Instant::now();
    let disc_result = state
        .process_manager
        .invoke_generic::<_, ApiGatewayProxyResponse>(&function_name, &disc_evt, timeout_ms)
        .await;
    let disc_latency = disc_start.elapsed().as_secs_f64() * 1000.0;
    let (disc_status, disc_healthy) = match &disc_result {
        Ok(r) => (r.status_code, r.status_code < 500),
        Err(_) => (0i64, false),
    };
    state
        .riz_state
        .record_invocation(&function_name, disc_latency, disc_healthy, false)
        .await;
    state.push_log(
        "INFO",
        Some(&function_name),
        format!(
            "WS $disconnect {disc_status} {disc_latency:.0}ms conn={} fn={function_name}",
            read_id
        ),
    );

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
