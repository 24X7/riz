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

/// Upper bound on retained task snapshots (Power of 10 rule 3, docs/SAFETY.md):
/// every SendMessage creates an entry that must survive for later GetTask
/// polling, so without a cap a remote client could grow the store without
/// limit. At the cap the oldest *terminal* task is evicted first; if every
/// retained task is still live, new work is rejected — running state is never
/// dropped.
const MAX_RETAINED_TASKS: usize = 1024;

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
    tasks: TaskStore,
    /// A2A client side: outgoing delegations to `[agent.peers]`.
    http: reqwest::Client,
    /// Peer Agent-Card descriptions, fetched lazily and cached (bounded by
    /// the config's `[agent.peers]` key set).
    peer_descriptions: tokio::sync::Mutex<std::collections::HashMap<String, String>>,
}

/// Bounded task store: a map for lookup plus an insertion-order queue that
/// drives eviction once [`MAX_RETAINED_TASKS`] is reached.
struct TaskStore {
    entries: DashMap<String, Arc<TaskEntry>>,
    order: Mutex<std::collections::VecDeque<String>>,
}

impl TaskStore {
    fn new() -> Self {
        Self {
            entries: DashMap::new(),
            order: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    fn get(&self, id: &str) -> Option<Arc<TaskEntry>> {
        self.entries.get(id).map(|e| e.value().clone())
    }

    /// Insert under the retention cap. At capacity the oldest terminal task
    /// is evicted; if every retained task is still live the insert is
    /// rejected instead — eviction must never drop running state.
    fn try_insert(&self, id: String, entry: Arc<TaskEntry>) -> Result<(), (i32, String)> {
        let Ok(mut order) = self.order.lock() else {
            return Err((-32603, "task store lock poisoned".to_string()));
        };
        if order.len() >= MAX_RETAINED_TASKS {
            let victim = order.iter().position(|tid| {
                // Entries missing from the map (shouldn't happen) count as
                // evictable so the queue can't wedge.
                self.entries
                    .get(tid)
                    .map(|e| is_terminal(e.value()))
                    .unwrap_or(true)
            });
            match victim {
                Some(idx) => {
                    if let Some(tid) = order.remove(idx) {
                        self.entries.remove(&tid);
                    }
                }
                None => {
                    return Err((
                        -32603,
                        format!("task store full: {MAX_RETAINED_TASKS} tasks still running"),
                    ));
                }
            }
        }
        order.push_back(id.clone());
        drop(order);
        self.entries.insert(id, entry);
        Ok(())
    }
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
                    str_field(&s, "id").to_string(),
                    str_field(&s, "contextId").to_string(),
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
            tasks: TaskStore::new(),
            http: reqwest::Client::new(),
            peer_descriptions: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    // ───────────────────────── Agent Card ───────────────────────────────────

    /// The Agent Card (spec §5): identity, endpoint, capabilities, and one
    /// skill per tool the agent may wield — derived LIVE from the same
    /// `tools/list` any MCP client sees, filtered by the allowlist.
    pub async fn agent_card(self: &Arc<Self>) -> serde_json::Value {
        let skills: Vec<serde_json::Value> = match self.allowed_mcp_tools().await {
            Ok(tools) => tools
                .into_iter()
                .map(|t| {
                    json!({
                        "id": t.get("name").cloned().unwrap_or_default(),
                        "name": t.get("name").cloned().unwrap_or_default(),
                        "description": t.get("description").cloned().unwrap_or_default(),
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
            // `card` is built as an object just above; the if-let is the
            // non-panicking spelling of that invariant.
            if let Some(obj) = card.as_object_mut() {
                obj.insert(
                    "securitySchemes".into(),
                    json!({ "bearer": { "type": "http", "scheme": "bearer" } }),
                );
                obj.insert("security".into(), json!([{ "bearer": [] }]));
            }
        }
        card
    }

    fn tool_allowed(&self, name: &str) -> bool {
        self.cfg.tools.is_empty() || self.cfg.tools.iter().any(|t| t == name)
    }

    /// The live MCP tool surface (`tools/list` through the Router), filtered
    /// by the `[agent]` allowlist — the raw MCP tool objects.
    async fn allowed_mcp_tools(&self) -> Result<Vec<serde_json::Value>, String> {
        let listed = self
            .mcp_rpc(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await?;
        Ok(listed
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| self.tool_allowed(str_field(t, "name")))
            .collect())
    }

    // ───────────────────────── JSON-RPC surface ─────────────────────────────

    pub async fn handle(self: &Arc<Self>, raw: serde_json::Value, hop: u32) -> serde_json::Value {
        let id = raw.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = str_field(&raw, "method");
        let params = raw.get("params").cloned().unwrap_or(json!({}));
        // Mesh loop protection: a delegation chain at or past max_hops is
        // rejected outright instead of spawning another agent loop.
        if hop >= self.cfg.max_hops && matches!(method, "SendMessage" | "message/send") {
            return json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": format!(
                    "a2a hop limit reached (max_hops = {}): refusing further delegation",
                    self.cfg.max_hops
                )}
            });
        }
        let result = match method {
            "SendMessage" | "message/send" => self.send_message(params, hop).await,
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
        hop: u32,
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
        let user_text: String = message
            .get("parts")
            .and_then(serde_json::Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
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
        self.tasks.try_insert(task_id.clone(), entry.clone())?;

        // Run the loop in its own task so CancelTask can abort it and slow
        // tasks outlive the SendMessage timeout (poll with GetTask).
        let rt = Arc::clone(self);
        let run_entry = entry.clone();
        let run_task_id = task_id.clone();
        let handle = tokio::spawn(async move {
            rt.run_agent_loop(&run_task_id, &run_entry, user_text, message, hop)
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
        hop: u32,
    ) -> Result<serde_json::Value, (i32, String)> {
        let (entry, _events) = self.start_task(params, hop)?;

        // Wait for a terminal state up to task_timeout_ms, then return the
        // snapshot either way (WORKING = keep polling).
        let deadline = deadline_after(std::time::Duration::from_millis(self.cfg.task_timeout_ms));
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
        hop: u32,
    ) -> Result<
        impl futures_util::Stream<
                Item = Result<axum::response::sse::Event, std::convert::Infallible>,
            > + Send
            + 'static,
        (i32, String),
    > {
        if hop >= self.cfg.max_hops {
            return Err((
                -32603,
                format!(
                    "a2a hop limit reached (max_hops = {}): refusing further delegation",
                    self.cfg.max_hops
                ),
            ));
        }
        let (entry, events) = self.start_task(params, hop)?;
        let initial = entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default();
        let timeout = std::time::Duration::from_millis(self.cfg.task_timeout_ms);

        Ok(stream_frames(initial, events, rpc_id, timeout))
    }

    fn get_task(&self, params: &serde_json::Value) -> Result<serde_json::Value, (i32, String)> {
        let id = str_field(params, "id");
        let entry = self
            .tasks
            .get(id)
            .ok_or((TASK_NOT_FOUND, format!("task not found: {id}")))?;
        Ok(entry.snapshot.lock().map(|s| s.clone()).unwrap_or_default())
    }

    fn cancel_task(&self, params: &serde_json::Value) -> Result<serde_json::Value, (i32, String)> {
        let id = str_field(params, "id");
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
        incoming_hop: u32,
    ) {
        set_state(entry, "working");

        let tools = match self.agent_tool_definitions().await {
            Ok(t) => t,
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
            // Boundary check (rule 5): the provider answered over the network;
            // an empty choices array fails the task instead of panicking.
            let Some(choice) = resp.choices.first() else {
                fail(entry, "gateway returned no choices");
                return;
            };
            let assistant = serde_json::to_value(&choice.message).unwrap_or(json!({}));
            messages.push(assistant);

            if choice.finish_reason != "tool_calls" || choice.message.tool_calls.is_empty() {
                let answer = choice.message.text_content().to_string();
                complete(entry, task_id, &user_message, answer);
                return;
            }
            for call in &choice.message.tool_calls {
                let result_text = self.execute_tool_call(call, &user_text, incoming_hop).await;
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

    /// Tool definitions for the agent loop: the live MCP tool surface
    /// (allowlist-filtered) mapped to the OpenAI shape the gateway speaks,
    /// plus one `delegate_to_<name>` tool per `[agent.peers]` entry (the mesh).
    async fn agent_tool_definitions(&self) -> Result<Vec<serde_json::Value>, String> {
        let mut tools: Vec<serde_json::Value> = self
            .allowed_mcp_tools()
            .await?
            .into_iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").cloned().unwrap_or_default(),
                        "description": t.get("description").cloned().unwrap_or_default(),
                        "parameters": t.get("inputSchema").cloned().unwrap_or_default(),
                    }
                })
            })
            .collect();
        for peer in self.cfg.peers.keys() {
            let description = self.peer_description(peer).await;
            tools.push(json!({
                "type": "function",
                "function": {
                    "name": format!("delegate_to_{peer}"),
                    "description": description,
                    "parameters": {
                        "type": "object",
                        "properties": { "message": {
                            "type": "string",
                            "description": "What to ask the peer agent. Omit to forward the user's request."
                        }},
                    }
                }
            }));
        }
        Ok(tools)
    }

    /// Execute one model tool call: `delegate_to_<peer>` goes out over A2A one
    /// hop deeper; anything else dispatches through the in-process MCP
    /// surface. Failures come back as text — a failing tool is information
    /// for the model, not a task-fatal error.
    async fn execute_tool_call(
        &self,
        call: &crate::llm::types::ToolCall,
        user_text: &str,
        incoming_hop: u32,
    ) -> String {
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(json!({}));
        let peer = call
            .function
            .name
            .strip_prefix("delegate_to_")
            .filter(|p| self.cfg.peers.contains_key(*p));
        if let Some(peer) = peer {
            // Mesh delegation: SendMessage to the peer, one hop deeper.
            // No explicit message → forward the user's request. saturating:
            // a saturated hop count is >= any max_hops, so the peer rejects
            // it — exactly the loop-protection semantic wanted here.
            let ask = args
                .get("message")
                .and_then(serde_json::Value::as_str)
                .filter(|m| !m.is_empty())
                .unwrap_or(user_text)
                .to_string();
            match self
                .delegate(peer, &ask, incoming_hop.saturating_add(1))
                .await
            {
                Ok(answer) => answer,
                Err(e) => format!("tool error: {e}"),
            }
        } else {
            match self
                .mcp_rpc(json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": call.function.name, "arguments": args }
                }))
                .await
            {
                Ok(result) => result
                    .pointer("/content/0/text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Err(e) => format!("tool error: {e}"),
            }
        }
    }

    // ───────────────────────── A2A client (the mesh) ────────────────────────

    /// Peer tool description: the peer's Agent Card identity when reachable
    /// (fetched once, cached), else a generic line — the peer may simply not
    /// be up yet, and delegation can still succeed later.
    async fn peer_description(&self, peer: &str) -> String {
        if let Some(d) = self.peer_descriptions.lock().await.get(peer) {
            return d.clone();
        }
        let Some(url) = self.cfg.peers.get(peer) else {
            return format!("Delegate this task to peer agent '{peer}'.");
        };
        let fetched = self
            .http
            .get(format!(
                "{}/.well-known/agent-card.json",
                url.trim_end_matches('/')
            ))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success());
        let description = match fetched {
            Some(resp) => match resp.json::<serde_json::Value>().await {
                Ok(card) => format!(
                    "Delegate this task to agent '{}': {}",
                    card.get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(peer),
                    str_field(&card, "description")
                ),
                Err(_) => format!("Delegate this task to peer agent '{peer}'."),
            },
            None => return format!("Delegate this task to peer agent '{peer}'."),
        };
        self.peer_descriptions
            .lock()
            .await
            .insert(peer.to_string(), description.clone());
        description
    }

    /// SendMessage to a peer and return its answer text (the completed task's
    /// artifact). The `riz-a2a-hop` header carries the chain depth; the peer
    /// rejects at its own max_hops — mesh loop protection.
    async fn delegate(&self, peer: &str, message: &str, hop: u32) -> Result<String, String> {
        let url = self
            .cfg
            .peers
            .get(peer)
            .ok_or_else(|| format!("unknown peer '{peer}'"))?;
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
            "params": { "message": {
                "kind": "message", "role": "user",
                "messageId": uuid::Uuid::new_v4().to_string(),
                "parts": [{ "kind": "text", "text": message }],
            }}
        });
        let resp = self
            .http
            .post(format!("{}/_riz/a2a", url.trim_end_matches('/')))
            .header("riz-a2a-hop", hop.to_string())
            .json(&body)
            .timeout(std::time::Duration::from_millis(self.cfg.task_timeout_ms))
            .send()
            .await
            .map_err(|e| format!("peer '{peer}' unreachable: {e}"))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("peer '{peer}' answered non-JSON: {e}"))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(format!(
                "peer '{peer}': {}",
                err.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("a2a error")
            ));
        }
        let task = v.get("result").cloned().unwrap_or(serde_json::Value::Null);
        let state = task
            .pointer("/status/state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if state != "completed" {
            return Err(format!(
                "peer '{peer}' task ended in state '{state}' (task id {})",
                task.get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ));
        }
        Ok(task
            .pointer("/artifacts/0/parts/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
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
            return Err(err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("mcp error")
                .to_string());
        }
        Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
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

/// `v[key]` as a str, `""` when absent or not a string — the non-panicking
/// spelling of the JSON-indexing idiom this module used to use.
fn str_field<'v>(v: &'v serde_json::Value, key: &str) -> &'v str {
    v.get(key).and_then(serde_json::Value::as_str).unwrap_or("")
}

/// `now + timeout` as a tokio Instant, saturating: a pathological
/// `task_timeout_ms` that would overflow the platform's time representation
/// clamps to ~1 day instead of panicking (rule 5: explicit recovery).
fn deadline_after(timeout: std::time::Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(timeout)
        .or_else(|| now.checked_add(std::time::Duration::from_secs(86_400)))
        .unwrap_or(now)
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
            deadline: deadline_after(timeout),
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
                        if event.get("final") == Some(&serde_json::Value::Bool(true)) {
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
                s.pointer("/status/state")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                "completed" | "failed" | "canceled" | "rejected"
            )
        })
        .unwrap_or(false)
}

fn set_state(entry: &TaskEntry, state: &str) {
    let status = json!({ "state": state, "timestamp": now_iso() });
    set_snapshot_field(entry, "status", status.clone());
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

/// Write one top-level field of the task snapshot. The snapshot is always
/// built by `task_value` (an object); the if-lets are the non-panicking
/// spelling of that invariant.
fn set_snapshot_field(entry: &TaskEntry, key: &str, value: serde_json::Value) {
    if let Ok(mut s) = entry.snapshot.lock() {
        if let Some(obj) = s.as_object_mut() {
            obj.insert(key.to_string(), value);
        }
    }
}

fn fail(entry: &TaskEntry, message: &str) {
    let status = json!({
        "state": "failed",
        "timestamp": now_iso(),
        "message": { "role": "agent", "parts": [{ "kind": "text", "text": message }] },
    });
    set_snapshot_field(entry, "status", status.clone());
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
    set_snapshot_field(entry, "artifacts", json!([artifact]));
    set_snapshot_field(
        entry,
        "history",
        json!([
            user_message,
            {
                "kind": "message", "role": "agent",
                "messageId": uuid::Uuid::new_v4().to_string(),
                "parts": artifact.get("parts").cloned().unwrap_or_else(|| json!([])),
            }
        ]),
    );
    set_snapshot_field(entry, "status", status.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_in_state(state: &str) -> Arc<TaskEntry> {
        let (events, _rx) = tokio::sync::broadcast::channel(4);
        Arc::new(TaskEntry {
            snapshot: Mutex::new(task_value("t", "c", state, vec![], vec![])),
            changed: Arc::new(Notify::new()),
            events,
            handle: Mutex::new(None),
        })
    }

    #[test]
    fn task_store_rejects_insert_when_full_of_live_tasks() {
        let store = TaskStore::new();
        for i in 0..MAX_RETAINED_TASKS {
            store
                .try_insert(format!("task-{i}"), entry_in_state("working"))
                .expect("under cap");
        }
        let err = store
            .try_insert("one-too-many".into(), entry_in_state("working"))
            .expect_err("store full of live tasks must reject");
        assert_eq!(err.0, -32603);
        assert!(err.1.contains("task store full"), "got: {}", err.1);
        assert!(store.get("one-too-many").is_none());
    }

    #[test]
    fn task_store_evicts_oldest_terminal_task_at_cap() {
        let store = TaskStore::new();
        store
            .try_insert("done-old".into(), entry_in_state("completed"))
            .unwrap();
        for i in 1..MAX_RETAINED_TASKS {
            store
                .try_insert(format!("task-{i}"), entry_in_state("working"))
                .unwrap();
        }
        // At cap: the terminal task is evicted, the new one lands.
        store
            .try_insert("newest".into(), entry_in_state("working"))
            .expect("terminal eviction must make room");
        assert!(store.get("done-old").is_none(), "oldest terminal evicted");
        assert!(store.get("newest").is_some());
        assert!(store.get("task-1").is_some(), "live tasks survive");
    }

    #[test]
    fn deadline_after_saturates_instead_of_panicking() {
        // A pathological config timeout must not panic the request path.
        let d = deadline_after(std::time::Duration::from_millis(u64::MAX));
        assert!(d >= tokio::time::Instant::now());
        // The normal case still lands in the future.
        let normal = deadline_after(std::time::Duration::from_millis(50));
        assert!(normal > tokio::time::Instant::now());
    }

    #[test]
    fn is_terminal_matches_terminal_states_only() {
        for s in ["completed", "failed", "canceled", "rejected"] {
            assert!(is_terminal(&entry_in_state(s)), "{s} must be terminal");
        }
        for s in ["submitted", "working", ""] {
            assert!(!is_terminal(&entry_in_state(s)), "{s} must not be terminal");
        }
    }
}
