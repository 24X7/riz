//! WebSocket functions as MCP tools — ephemeral sessions.
//!
//! Spec: docs/superpowers/specs/2026-07-02-ws-ephemeral-tool-sessions-design.md
//!
//! `tools/call` on a WebSocket function opens a short-lived REAL session:
//! `$connect` → `$default(message)` → collect what the handler pushes through
//! the `@connections` API (the store entry is real, so pushes land in an
//! in-process collector instead of 410ing) → `$disconnect` → frames become
//! the tool result. Every part of the WS contract behaves normally; the agent
//! just experiences it as a slightly slower tool.
//!
//! Reply semantics mirror the real socket path exactly: riz never relays the
//! `$default` response body to a socket — replies flow ONLY through
//! `@connections` pushes — so the session collects only pushes and reports
//! the `$default` status code in `structuredContent`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use tokio::sync::mpsc;

use crate::gateway::ApiGatewayProxyResponse;
use crate::process::ProcessManager;
use crate::ws::connection::{Connection, ConnectionId, OutboundMessage};
use crate::ws::event::{build_connect, build_disconnect, build_message};
use crate::ws::ConnectionStore;

use super::protocol::{JsonRpcError, ToolArguments};
use super::McpHandler;

/// Everything the session runner needs from the app — wired post-construction
/// (like the Router) because McpHandler is built before AppState assembles.
#[derive(Clone)]
pub struct WsSessionDeps {
    pub process_manager: Arc<ProcessManager>,
    pub connections: ConnectionStore,
    pub stage: String,
}

/// Default and ceiling for how long a session waits on a silent handler.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 30_000;

/// The tool input schema every WS function advertises.
pub(super) fn session_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "Delivered to the function as the $default event body."
            },
            "timeout_ms": {
                "type": "integer",
                "description": format!(
                    "How long to wait for reply frames if none arrive by the time \
                     the handler returns (default {DEFAULT_TIMEOUT_MS}, max {MAX_TIMEOUT_MS})."
                )
            }
        },
        "required": ["message"]
    })
}

pub(super) fn session_description(function_name: &str) -> String {
    format!(
        "Open an ephemeral WebSocket session with function `{function_name}`: \
         delivers `message` as a $default event ($connect/$disconnect fire \
         normally) and returns the frames the handler pushes via @connections."
    )
}

/// The result shape a session returns in `structuredContent`.
pub(super) fn session_output_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "frames": {
                "type": "array",
                "description": "Frames the handler pushed via @connections during the session."
            },
            "connection_id": { "type": "string" },
            "default_status": { "type": "integer" }
        },
        "required": ["frames"]
    })
}

/// Removes the collector connection from the store on every exit path.
struct SessionGuard {
    connections: ConnectionStore,
    id: ConnectionId,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.connections.remove(&self.id);
    }
}

impl McpHandler {
    pub(super) async fn tools_call_ws_session(
        &self,
        function_name: &str,
        fn_timeout_ms: u64,
        arguments: &ToolArguments,
    ) -> Result<serde_json::Value, JsonRpcError> {
        // Caller errors first (they hold regardless of wiring), then runtime.
        let message = arguments.message.clone().ok_or_else(|| JsonRpcError {
            code: -32602,
            message: "missing required parameter 'message' (string)".into(),
        })?;
        let wait_ms = arguments
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);
        let deps = self
            .ws_session_deps
            .read()
            .await
            .clone()
            .ok_or_else(|| JsonRpcError {
                code: -32603,
                message: "WebSocket sessions unavailable: runtime not wired".into(),
            })?;

        let connection_id = ConnectionId::new();
        let connected_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        // 1. $connect — a non-200 rejects the session, like a real upgrade.
        let connect_evt = build_connect(
            &deps.stage,
            connection_id.as_str(),
            connected_at_ms,
            "/",
            http::HeaderMap::new(),
            std::collections::HashMap::new(),
        );
        let connect_resp: ApiGatewayProxyResponse = deps
            .process_manager
            .invoke_generic(function_name, &connect_evt, fn_timeout_ms)
            .await
            .map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("$connect failed for '{function_name}': {e}"),
            })?;
        if connect_resp.status_code != 200 {
            return Err(JsonRpcError {
                code: -32603,
                message: format!(
                    "'{function_name}' rejected the session: $connect returned {}",
                    connect_resp.status_code
                ),
            });
        }

        // 2. Register the collector-backed connection. Pushes to
        //    /_riz/connections/{id} land in `rx` because this entry is real.
        let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();
        let now = Instant::now();
        let conn = Arc::new(Connection {
            id: connection_id.clone(),
            function_name: function_name.to_string(),
            connected_at: now,
            last_active: std::sync::Mutex::new(now),
            outbound: tx,
            close_signal: std::sync::Mutex::new(None),
        });
        if let Err(reason) = deps.connections.try_insert(conn) {
            return Err(JsonRpcError {
                code: -32603,
                message: format!("session rejected: {reason}"),
            });
        }
        let _guard = SessionGuard {
            connections: deps.connections.clone(),
            id: connection_id.clone(),
        };

        // 3. $default with the message. Pushes made DURING the invocation are
        //    already queued in rx when it returns (same-process HTTP).
        let msg_evt = build_message(
            &deps.stage,
            connection_id.as_str(),
            connected_at_ms,
            Some(message),
            false,
        );
        let default_result: Result<ApiGatewayProxyResponse, _> = deps
            .process_manager
            .invoke_generic(function_name, &msg_evt, fn_timeout_ms)
            .await;
        let default_status = match &default_result {
            Ok(r) => r.status_code,
            Err(e) => {
                // Still fire $disconnect below via the guard-less best-effort
                // call, then surface the dispatch failure.
                let _ = Self::fire_disconnect(
                    &deps,
                    function_name,
                    &connection_id,
                    connected_at_ms,
                    fn_timeout_ms,
                )
                .await;
                return Err(JsonRpcError {
                    code: -32603,
                    message: format!("$default failed for '{function_name}': {e}"),
                });
            }
        };

        // 4. Collect. Deterministic rule: drain what's queued; if nothing
        //    arrived by the time the handler returned, wait up to `wait_ms`
        //    for the FIRST async push, then drain whatever came with it.
        let mut frames: Vec<serde_json::Value> = Vec::new();
        let mut closed = false;
        drain(&mut rx, &mut frames, &mut closed);
        if frames.is_empty() && !closed {
            if let Ok(Some(first)) =
                tokio::time::timeout(Duration::from_millis(wait_ms), rx.recv()).await
            {
                push_frame(first, &mut frames, &mut closed);
                drain(&mut rx, &mut frames, &mut closed);
            }
        }

        // 5. $disconnect — best-effort, always.
        let _ = Self::fire_disconnect(
            &deps,
            function_name,
            &connection_id,
            connected_at_ms,
            fn_timeout_ms,
        )
        .await;

        let text = frames
            .iter()
            .filter_map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(serde_json::json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": {
                "frames": frames,
                "connection_id": connection_id.as_str(),
                "default_status": default_status,
            },
            "isError": false,
        }))
    }

    async fn fire_disconnect(
        deps: &WsSessionDeps,
        function_name: &str,
        connection_id: &ConnectionId,
        connected_at_ms: i64,
        fn_timeout_ms: u64,
    ) -> Result<ApiGatewayProxyResponse, crate::process::PoolError> {
        let evt = build_disconnect(&deps.stage, connection_id.as_str(), connected_at_ms);
        deps.process_manager
            .invoke_generic(function_name, &evt, fn_timeout_ms)
            .await
    }
}

fn push_frame(msg: OutboundMessage, frames: &mut Vec<serde_json::Value>, closed: &mut bool) {
    match msg {
        OutboundMessage::Text(s) => frames.push(serde_json::Value::String(s)),
        OutboundMessage::Binary(b) => frames.push(serde_json::json!({
            "base64": base64::engine::general_purpose::STANDARD.encode(b)
        })),
        OutboundMessage::Close => *closed = true,
    }
}

fn drain(
    rx: &mut mpsc::UnboundedReceiver<OutboundMessage>,
    frames: &mut Vec<serde_json::Value>,
    closed: &mut bool,
) {
    while let Ok(msg) = rx.try_recv() {
        push_frame(msg, frames, closed);
    }
}
