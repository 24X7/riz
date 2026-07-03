//! A2A (a2a-protocol.org) server — riz as a built-in agent.
//!
//! Spec: docs/superpowers/specs/2026-07-02-a2a-builtin-agent-design.md
//!
//! `[agent]` in riz.toml turns this instance into an agent2agent server:
//! an Agent Card at `/.well-known/agent-card.json` and a JSON-RPC binding at
//! `POST /_riz/a2a` (`SendMessage` / `SendStreamingMessage` — SSE with live
//! status + artifact events / `GetTask` / `CancelTask`; 0.x aliases accepted). A
//! delegated task runs the agent loop: gateway chat with this instance's OWN
//! functions as tools — the tool definitions and execution both go through
//! the in-process MCP surface (`tools/list` / `tools/call` dispatched through
//! the Router), so the A2A agent wields exactly the typed tools any external
//! MCP client sees, including WebSocket session tools.
//!
//! The mock provider drives the full loop deterministically, so delegate →
//! reason → act → answer is provable offline in CI with zero keys.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use serde_json::json;
use tokio::sync::Notify;

use crate::config::AgentConfig;
use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
};
use crate::llm::{ChatRequest, Gateway};
use crate::state::AppState;

/// A2A JSON-RPC error codes (spec §8).
const TASK_NOT_FOUND: i32 = -32001;
const TASK_NOT_CANCELABLE: i32 = -32002;

/// Everything the A2A surface needs, assembled at mount time (server.rs).
pub struct A2aRuntime {
    pub cfg: AgentConfig,
    gateway: Arc<Gateway>,
    app: Arc<AppState>,
    /// Forwarded on internal MCP dispatches so the agent works when `/_riz/*`
    /// is bearer-gated.
    bearer: Option<String>,
    /// Base for the Agent Card's service endpoint URL.
    public_base: String,
    tasks: DashMap<String, Arc<TaskEntry>>,
}

struct TaskEntry {
    /// The evolving A2A Task object, serialized shape (spec §4).
    snapshot: Mutex<serde_json::Value>,
    /// Fires on every state change; SendMessage waits on it with a timeout.
    changed: Arc<Notify>,
    /// Live event feed for SendStreamingMessage subscribers: status-update /
    /// artifact-update frames (spec §7). Lagged subscribers drop old frames
    /// but always still see the terminal event.
    events: tokio::sync::broadcast::Sender<serde_json::Value>,
    /// The running loop — CancelTask aborts it.
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TaskEntry {
    fn ids(&self) -> (String, String) {
        self.snapshot
            .lock()
            .map(|s| {
                (
                    s["id"].as_str().unwrap_or("").to_string(),
                    s["contextId"].as_str().unwrap_or("").to_string(),
                )
            })
            .unwrap_or_default()
    }

    fn emit(&self, event: serde_json::Value) {
        let _ = self.events.send(event);
    }
}

impl A2aRuntime {
    pub fn new(
        cfg: AgentConfig,
        gateway: Arc<Gateway>,
        app: Arc<AppState>,
        bearer: Option<String>,
        public_base: String,
    ) -> Self {
        Self {
            cfg,
            gateway,
            app,
            bearer,
            public_base,
            tasks: DashMap::new(),
        }
    }

    // ───────────────────────── Agent Card ───────────────────────────────────

    /// The Agent Card (spec §5): identity, endpoint, capabilities, and one
    /// skill per tool the agent may wield — derived LIVE from the same
    /// `tools/list` any MCP client sees, filtered by the allowlist.
    pub async fn agent_card(self: &Arc<Self>) -> serde_json::Value {
        let skills: Vec<serde_json::Value> = match self
            .mcp_rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list"
            }))
            .await
        {
            Ok(result) => result["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| self.tool_allowed(t["name"].as_str().unwrap_or("")))
                .map(|t| {
                    json!({
                        "id": t["name"],
                        "name": t["name"],
                        "description": t["description"],
                        "tags": ["riz-function"],
                    })
                })
                .collect(),
            Err(_) => vec![],
        };
        let mut card = json!({
            "protocolVersion": "1.0",
            "name": self.cfg.name,
            "description": self.cfg.description,
            "url": format!("{}/_riz/a2a", self.public_base),
            "preferredTransport": "JSONRPC",
            "version": env!("CARGO_PKG_VERSION"),
            "capabilities": { "streaming": true, "pushNotifications": false },
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": skills,
        });
        if self.bearer.is_some() {
            card["securitySchemes"] = json!({ "bearer": { "type": "http", "scheme": "bearer" } });
            card["security"] = json!([{ "bearer": [] }]);
        }
        card
    }

    fn tool_allowed(&self, name: &str) -> bool {
        self.cfg.tools.is_empty() || self.cfg.tools.iter().any(|t| t == name)
    }

    // ───────────────────────── JSON-RPC surface ─────────────────────────────

    pub async fn handle(self: &Arc<Self>, raw: serde_json::Value) -> serde_json::Value {
        let id = raw.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = raw["method"].as_str().unwrap_or("");
        let params = raw.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "SendMessage" | "message/send" => self.send_message(params).await,
            "GetTask" | "tasks/get" => self.get_task(&params),
            "CancelTask" | "tasks/cancel" => self.cancel_task(&params),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message }
            }),
        }
    }

    /// Validate the params, create the task, and spawn its agent loop.
    /// Shared by SendMessage and SendStreamingMessage; the returned receiver
    /// was subscribed BEFORE the loop spawned, so no event can be missed.
    fn start_task(
        self: &Arc<Self>,
        params: serde_json::Value,
    ) -> Result<
        (
            Arc<TaskEntry>,
            tokio::sync::broadcast::Receiver<serde_json::Value>,
        ),
        (i32, String),
    > {
        let message = params
            .get("message")
            .cloned()
            .ok_or((-32602, "missing required parameter: message".to_string()))?;
        let user_text: String = message["parts"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if user_text.is_empty() {
            return Err((
                -32602,
                "message.parts must contain at least one text part".to_string(),
            ));
        }

        let task_id = uuid::Uuid::new_v4().to_string();
        let context_id = uuid::Uuid::new_v4().to_string();
        let (events, receiver) = tokio::sync::broadcast::channel(64);
        let entry = Arc::new(TaskEntry {
            snapshot: Mutex::new(task_value(
                &task_id,
                &context_id,
                "submitted",
                vec![],
                vec![message.clone()],
            )),
            changed: Arc::new(Notify::new()),
            events,
            handle: Mutex::new(None),
        });
        self.tasks.insert(task_id.clone(), entry.clone());

        // Run the loop in its own task so CancelTask can abort it and slow
        // tasks outlive the SendMessage timeout (poll with GetTask).
        let rt = Arc::clone(self);
        let run_entry = entry.clone();
        let run_task_id = task_id.clone();
        let handle = tokio::spawn(async move {
            rt.run_agent_loop(&run_task_id, &run_entry, user_text, message)
                .await;
        });
        if let Ok(mut h) = entry.handle.lock() {
            *h = Some(handle);
        }
        Ok((entry, receiver))
    }

    async fn send_message(
        self: &Arc<Self>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, (i32, String)> {
        let (entry, _events) = self.start_task(params)?;

        // Wait for a terminal state up to task_timeout_ms, then return the
        // snapshot either way (WORKING = keep polling).
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(self.cfg.task_timeout_ms);
        loop {
            if is_terminal(&entry) {
                break;
            }
            let notified = entry.changed.notified();
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        Ok(entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default())
    }

    /// SendStreamingMessage: the SSE frame sequence (spec §7) — the initial
    /// Task snapshot, then live status-update / artifact-update events, ending
    /// with the terminal status-update (`final: true`).
    pub async fn send_streaming(
        self: &Arc<Self>,
        params: serde_json::Value,
        rpc_id: serde_json::Value,
    ) -> Result<
        impl futures_util::Stream<
                Item = Result<axum::response::sse::Event, std::convert::Infallible>,
            > + Send
            + 'static,
        (i32, String),
    > {
        let (entry, events) = self.start_task(params)?;
        let initial = entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default();
        let timeout = std::time::Duration::from_millis(self.cfg.task_timeout_ms);

        Ok(stream_frames(initial, events, rpc_id, timeout))
    }

    fn get_task(&self, params: &serde_json::Value) -> Result<serde_json::Value, (i32, String)> {
        let id = params["id"].as_str().unwrap_or("");
        let entry = self
            .tasks
            .get(id)
            .ok_or((TASK_NOT_FOUND, format!("task not found: {id}")))?;
        Ok(entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default())
    }

    fn cancel_task(&self, params: &serde_json::Value) -> Result<serde_json::Value, (i32, String)> {
        let id = params["id"].as_str().unwrap_or("");
        let entry = self
            .tasks
            .get(id)
            .ok_or((TASK_NOT_FOUND, format!("task not found: {id}")))?;
        if is_terminal(&entry) {
            return Err((
                TASK_NOT_CANCELABLE,
                format!("task {id} already reached a terminal state"),
            ));
        }
        if let Ok(mut h) = entry.handle.lock() {
            if let Some(handle) = h.take() {
                handle.abort();
            }
        }
        set_state(&entry, "canceled");
        Ok(entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default())
    }

    // ───────────────────────── The agent loop ───────────────────────────────

    /// messages → gateway chat (tools attached) → execute tool_calls through
    /// the in-process MCP surface → tool results back to the model → repeat
    /// until a text answer (→ artifact, COMPLETED) or the hop cap (FAILED).
    async fn run_agent_loop(
        self: Arc<Self>,
        task_id: &str,
        entry: &Arc<TaskEntry>,
        user_text: String,
        user_message: serde_json::Value,
    ) {
        set_state(entry, "working");

        // Tool definitions = the live MCP tool surface, allowlist-filtered,
        // mapped to the OpenAI shape the gateway speaks.
        let tools: Vec<serde_json::Value> = match self
            .mcp_rpc(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
        {
            Ok(result) => result["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| self.tool_allowed(t["name"].as_str().unwrap_or("")))
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t["name"],
                            "description": t["description"],
                            "parameters": t["inputSchema"],
                        }
                    })
                })
                .collect(),
            Err(e) => {
                fail(entry, &format!("tools/list failed: {e}"));
                return;
            }
        };

        let mut messages: Vec<serde_json::Value> = Vec::new();
        if let Some(system) = &self.cfg.system_prompt {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.push(json!({ "role": "user", "content": user_text }));

        for _hop in 0..self.cfg.max_hops {
            let req: ChatRequest = match serde_json::from_value(json!({
                "model": self.cfg.model,
                "messages": messages,
                "tools": tools,
            })) {
                Ok(r) => r,
                Err(e) => {
                    fail(entry, &format!("internal: chat request malformed: {e}"));
                    return;
                }
            };
            let resp = match self.gateway.chat(&req).await {
                Ok(r) => r,
                Err(e) => {
                    fail(entry, &format!("gateway: {e}"));
                    return;
                }
            };
            let choice = &resp.choices[0];
            let assistant = serde_json::to_value(&choice.message).unwrap_or(json!({}));
            messages.push(assistant);

            if choice.finish_reason != "tool_calls" || choice.message.tool_calls.is_empty() {
                let answer = choice.message.text_content().to_string();
                complete(entry, task_id, &user_message, answer);
                return;
            }
            for call in &choice.message.tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or(json!({}));
                let result_text = match self
                    .mcp_rpc(json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": { "name": call.function.name, "arguments": args }
                    }))
                    .await
                {
                    Ok(result) => result["content"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    // A failing tool is information for the model, not a
                    // task-fatal error — agents recover from tool errors.
                    Err(e) => format!("tool error: {e}"),
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": result_text,
                }));
            }
        }
        fail(
            entry,
            &format!("agent loop exceeded max_hops = {}", self.cfg.max_hops),
        );
    }

    // ───────────────────── In-process MCP dispatch ───────────────────────────

    /// POST a JSON-RPC message to this instance's own /_riz/mcp — through the
    /// Router, not TCP — and return the JSON-RPC `result` (or the error).
    async fn mcp_rpc(&self, rpc: serde_json::Value) -> Result<serde_json::Value, String> {
        let event = internal_mcp_event(&rpc, self.bearer.as_deref());
        let router = self.app.router.read().await;
        let outcome = router
            .dispatch(event)
            .await
            .map_err(|e| format!("mcp dispatch: {e}"))?;
        let body = match &outcome.response.body {
            Some(aws_lambda_events::encodings::Body::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("mcp response not JSON: {e}"))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(err["message"].as_str().unwrap_or("mcp error").to_string());
        }
        Ok(v["result"].clone())
    }
}

/// A synthetic AWS v2 event for the internal POST /_riz/mcp dispatch.
fn internal_mcp_event(rpc: &serde_json::Value, bearer: Option<&str>) -> ApiGatewayV2httpRequest {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    if let Some(tok) = bearer {
        if let Ok(v) = http::HeaderValue::from_str(&format!("Bearer {tok}")) {
            headers.insert(http::header::AUTHORIZATION, v);
        }
    }
    ApiGatewayV2httpRequest {
        headers,
        raw_path: Some("/_riz/mcp".into()),
        body: Some(rpc.to_string()),
        request_context: ApiGatewayV2httpRequestContext {
            http: ApiGatewayV2httpRequestContextHttpDescription {
                method: http::Method::POST,
                path: Some("/_riz/mcp".into()),
                protocol: Some("HTTP/1.1".into()),
                source_ip: Some("127.0.0.1".into()),
                user_agent: Some("riz-a2a-agent".into()),
            },
            ..Default::default()
        },
        http_method: http::Method::POST,
        ..Default::default()
    }
}

// ───────────────────────── Task shape helpers ────────────────────────────────

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn task_value(
    id: &str,
    context_id: &str,
    state: &str,
    artifacts: Vec<serde_json::Value>,
    history: Vec<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "kind": "task",
        "id": id,
        "contextId": context_id,
        "status": { "state": state, "timestamp": now_iso() },
        "artifacts": artifacts,
        "history": history,
    })
}

/// One SSE frame: a complete JSON-RPC response wrapping an A2A event.
fn sse_frame(rpc_id: &serde_json::Value, result: serde_json::Value) -> axum::response::sse::Event {
    axum::response::sse::Event::default()
        .data(json!({ "jsonrpc": "2.0", "id": rpc_id, "result": result }).to_string())
}

/// The SendStreamingMessage frame sequence: the initial Task snapshot, then
/// every task event until the terminal one (`final: true`), the subscription
/// timeout, or channel close. On timeout the stream simply ends — the client
/// holds the task id from the initial frame and can poll GetTask.
fn stream_frames(
    initial: serde_json::Value,
    events: tokio::sync::broadcast::Receiver<serde_json::Value>,
    rpc_id: serde_json::Value,
    timeout: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
       + Send
       + 'static {
    struct St {
        events: tokio::sync::broadcast::Receiver<serde_json::Value>,
        rpc_id: serde_json::Value,
        deadline: tokio::time::Instant,
        initial: Option<serde_json::Value>,
        done: bool,
    }
    futures_util::stream::unfold(
        St {
            events,
            rpc_id,
            deadline: tokio::time::Instant::now() + timeout,
            initial: Some(initial),
            done: false,
        },
        |mut s| async move {
            if s.done {
                return None;
            }
            if let Some(initial) = s.initial.take() {
                return Some((Ok(sse_frame(&s.rpc_id, initial)), s));
            }
            loop {
                match tokio::time::timeout_at(s.deadline, s.events.recv()).await {
                    Ok(Ok(event)) => {
                        if event["final"] == json!(true) {
                            s.done = true;
                        }
                        return Some((Ok(sse_frame(&s.rpc_id, event)), s));
                    }
                    // Lagged: skip to the newest events — the terminal frame
                    // is what matters and is never overwritten.
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return None,
                    Err(_elapsed) => return None,
                }
            }
        },
    )
}

fn is_terminal(entry: &TaskEntry) -> bool {
    entry
        .snapshot
        .lock()
        .map(|s| {
            matches!(
                s["status"]["state"].as_str().unwrap_or(""),
                "completed" | "failed" | "canceled" | "rejected"
            )
        })
        .unwrap_or(false)
}

fn set_state(entry: &TaskEntry, state: &str) {
    let status = json!({ "state": state, "timestamp": now_iso() });
    if let Ok(mut s) = entry.snapshot.lock() {
        s["status"] = status.clone();
    }
    let (task_id, context_id) = entry.ids();
    let is_final = matches!(state, "completed" | "failed" | "canceled" | "rejected");
    entry.emit(json!({
        "kind": "status-update",
        "taskId": task_id,
        "contextId": context_id,
        "status": status,
        "final": is_final,
    }));
    entry.changed.notify_waiters();
}

fn fail(entry: &TaskEntry, message: &str) {
    let status = json!({
        "state": "failed",
        "timestamp": now_iso(),
        "message": { "role": "agent", "parts": [{ "kind": "text", "text": message }] },
    });
    if let Ok(mut s) = entry.snapshot.lock() {
        s["status"] = status.clone();
    }
    let (task_id, context_id) = entry.ids();
    entry.emit(json!({
        "kind": "status-update",
        "taskId": task_id,
        "contextId": context_id,
        "status": status,
        "final": true,
    }));
    entry.changed.notify_waiters();
}

fn complete(entry: &TaskEntry, task_id: &str, user_message: &serde_json::Value, answer: String) {
    let artifact = json!({
        "artifactId": format!("{task_id}-answer"),
        "name": "answer",
        "parts": [{ "kind": "text", "text": answer }],
    });
    let status = json!({ "state": "completed", "timestamp": now_iso() });
    if let Ok(mut s) = entry.snapshot.lock() {
        s["artifacts"] = json!([artifact]);
        s["history"] = json!([
            user_message,
            {
                "kind": "message", "role": "agent",
                "messageId": uuid::Uuid::new_v4().to_string(),
                "parts": artifact["parts"].clone(),
            }
        ]);
        s["status"] = status.clone();
    }
    let (tid, context_id) = entry.ids();
    entry.emit(json!({
        "kind": "artifact-update",
        "taskId": tid,
        "contextId": context_id,
        "artifact": artifact,
        "lastChunk": true,
    }));
    entry.emit(json!({
        "kind": "status-update",
        "taskId": tid,
        "contextId": context_id,
        "status": status,
        "final": true,
    }));
    entry.changed.notify_waiters();
}
