//! /_riz/mcp handler — full MCP-spec-compliant JSON-RPC 2.0 server.
//!
//! Supports the lifecycle (`initialize`, `notifications/initialized`, `ping`),
//! tools (`tools/list`, `tools/call`), resources (`resources/list` +
//! `resources/read` — the live function registry and a generated llms.txt,
//! see resources.rs), and an empty `prompts/list` so probing clients don't
//! error.
//!
//! Each user function in the riz.toml becomes one MCP tool. tools/call
//! assembles a ApiGatewayV2httpRequest from the supplied arguments and dispatches it
//! through the Router — so any function becomes MCP-callable with no changes
//! to the function's own code.
//!
//! Transport: stateless HTTP. One JSON-RPC message per POST. Notifications
//! (requests without `id`) get a 202 Accepted (Streamable HTTP spec). Batch arrays are
//! still accepted for legacy 2024-11-05 / 2025-03-26 clients — batching was
//! removed in MCP 2025-06-18, and new clients should send single messages.
//!
//! Protocol version: defaults to **2025-11-25** (current stable). On
//! `initialize`, echoes the version requested by the client if it appears in
//! `SUPPORTED_PROTOCOL_VERSIONS` (currently 2024-11-05, 2025-03-26,
//! 2025-06-18, 2025-11-25); otherwise responds with the server default and
//! lets the client decide whether to proceed.

mod encoding;
mod protocol;
mod resources;
mod schema;
mod tools;
pub mod transport;

use crate::auth::bearer::validate_bearer;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::router::Router;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::RizState;
use async_trait::async_trait;
use std::sync::Arc;

use crate::runtime::response::json_response as http_json_response;
use encoding::{accepted_response, json_response, jsonrpc_error_response, jsonrpc_error_value};
use protocol::{JsonRpcError, JsonRpcRequest};

pub struct McpHandler {
    routes: Vec<RouteEntry>,
    pub(super) riz_state: Arc<RizState>,
    pub(super) router: tokio::sync::RwLock<Option<Arc<Router>>>,
    bearer_token: Option<String>,
}

impl McpHandler {
    pub fn new(riz_state: Arc<RizState>, bearer_token: Option<String>) -> Self {
        Self {
            routes: vec![
                // POST is the JSON-RPC + Streamable-HTTP request path.
                RouteEntry {
                    method: RouteMethod::Post,
                    path: "/_riz/mcp".into(),
                },
                // GET on the MCP endpoint is part of the Streamable HTTP
                // transport spec (2025-03-26+): clients use it to subscribe to
                // server-initiated SSE streams. GET *with* `Accept:
                // text/event-stream` is served at the axum layer (see
                // transport.rs — a live SSE channel); a GET without that
                // accept header lands here and gets 405 with a JSON-RPC
                // method-not-allowed shape. Without this route the gateway
                // would 404, which clients can't disambiguate from
                // "endpoint missing".
                RouteEntry {
                    method: RouteMethod::Get,
                    path: "/_riz/mcp".into(),
                },
            ],
            riz_state,
            router: tokio::sync::RwLock::new(None),
            bearer_token,
        }
    }

    /// Called after Router construction (chicken-and-egg: McpHandler is one of
    /// the things the Router holds, and it dispatches reentrantly through it).
    pub async fn set_router(&self, router: Arc<Router>) {
        *self.router.write().await = Some(router);
    }
}

#[async_trait]
impl LambdaHandler for McpHandler {
    fn name(&self) -> &str {
        "POST /_riz/mcp"
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        // Auth check MUST be first — before any body parsing — so a wrong token
        // with a malformed body returns 401, not a JSON-RPC parse error.
        if let Some(expected) = &self.bearer_token {
            let auth_header = event
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            if !validate_bearer(auth_header, expected) {
                let path = event.raw_path.as_deref().unwrap_or("/_riz/mcp");
                let ip = event
                    .request_context
                    .http
                    .source_ip
                    .as_deref()
                    .unwrap_or("-");
                tracing::warn!(path = %path, source_ip = %ip, "unauthorized request");
                return Ok(http_json_response(
                    401,
                    &serde_json::json!({"error": "unauthorized"}),
                ));
            }
        }
        // Streamable HTTP (MCP 2025-03-26+): GET is reserved for server-initiated
        // SSE streams, which transport.rs serves when the client sends
        // `Accept: text/event-stream`. A GET that lands here lacked that
        // accept header → 405 with Allow: POST per HTTP semantics.
        if event.http_method == http::Method::GET {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": serde_json::Value::Null,
                "error": {
                    "code": -32601,
                    "message": "GET on /_riz/mcp requires Accept: text/event-stream (opens the SSE channel). Use POST with a JSON-RPC body for requests."
                }
            });
            let mut resp = http_json_response(405, &body);
            resp.headers
                .insert("allow", http::HeaderValue::from_static("POST"));
            return Ok(resp);
        }
        let body = event.body.as_deref().unwrap_or("{}");
        // Parse as raw JSON first to detect batch (array) vs single (object)
        let raw: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                return Ok(jsonrpc_error_response(
                    serde_json::Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ))
            }
        };

        // JSON-RPC 2.0 batch: array of requests. Process each, collect
        // non-notification responses, return a JSON array (or 202 if all
        // were notifications). Empty batch is itself an "Invalid Request".
        if let Some(arr) = raw.as_array() {
            if arr.is_empty() {
                return Ok(jsonrpc_error_response(
                    serde_json::Value::Null,
                    -32600,
                    "empty batch is invalid",
                ));
            }
            let mut out: Vec<serde_json::Value> = Vec::new();
            for item in arr {
                if let Some(resp) = self.process_one(item).await {
                    out.push(resp);
                }
            }
            return Ok(if out.is_empty() {
                accepted_response()
            } else {
                json_response(serde_json::Value::Array(out))
            });
        }

        // Single request (object).
        match self.process_one(&raw).await {
            Some(resp) => Ok(json_response(resp)),
            None => Ok(accepted_response()), // it was a notification
        }
    }
}

impl McpHandler {
    /// Process one JSON-RPC message. Returns Some(response JSON) for requests
    /// (those with an `id`); None for notifications.
    async fn process_one(&self, raw: &serde_json::Value) -> Option<serde_json::Value> {
        // Parse into JsonRpcRequest. On parse failure: if it looks like it had
        // an id, return an error response; otherwise (looks like a notification)
        // silently drop.
        let req: JsonRpcRequest = match serde_json::from_value(raw.clone()) {
            Ok(r) => r,
            Err(e) => {
                let id = raw.get("id").cloned().unwrap_or(serde_json::Value::Null);
                if raw.get("id").is_some() {
                    return Some(jsonrpc_error_value(
                        id,
                        -32600,
                        &format!("invalid request: {e}"),
                    ));
                }
                return None;
            }
        };

        let is_notification = req.id.is_none();
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        let result: Result<serde_json::Value, JsonRpcError> = match req.method.as_str() {
            // Lifecycle
            "initialize" => self.initialize(req.params).await,
            "notifications/initialized" => {
                // No response for notifications.
                return None;
            }
            "ping" => Ok(serde_json::json!({})),

            // Tools
            "tools/list" => self.tools_list_value().await,
            "tools/call" => self.tools_call_value(req.params).await,

            // Resources — the instance describes itself (live registry +
            // llms.txt). See resources.rs.
            "resources/list" => self.resources_list_value().await,
            "resources/read" => self.resources_read_value(req.params).await,
            "resources/templates/list" => Ok(serde_json::json!({ "resourceTemplates": [] })),

            // Prompts — not exposed, but an empty list keeps probing clients
            // from choking on -32601.
            "prompts/list" => Ok(serde_json::json!({ "prompts": [] })),

            // Unknown method
            other => Err(JsonRpcError {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        };

        if is_notification {
            // Per JSON-RPC 2.0 spec: notifications never receive a response,
            // even if processing produced an error.
            return None;
        }

        Some(match result {
            Ok(value) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": value,
            }),
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": e.code, "message": e.message },
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse, Body};
    use crate::state::{FunctionState, RizState};
    use crate::test_helpers::make_event_with_body;
    use encoding::substitute_path_params;
    use protocol::SERVER_DEFAULT_PROTOCOL_VERSION;
    use std::collections::HashMap;

    fn evt(body: &str) -> ApiGatewayV2httpRequest {
        make_event_with_body("POST", "/_riz/mcp", body)
    }

    fn evt_with_auth(body: &str, token: &str) -> ApiGatewayV2httpRequest {
        let mut e = make_event_with_body("POST", "/_riz/mcp", body);
        e.headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        e
    }

    fn body_text(resp: &ApiGatewayV2httpResponse) -> String {
        match resp.body.as_ref().expect("body") {
            Body::Text(s) => s.clone(),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    fn user_state() -> FunctionState {
        let c = crate::config::FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            env: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        };
        FunctionState::user("api", c, "$default", 0)
    }

    // ─── Auth tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn mcp_returns_401_when_token_required_and_missing() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, Some("secret".into()));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn mcp_returns_401_when_token_required_and_wrong() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, Some("secret".into()));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = h.invoke(evt_with_auth(req, "wrong")).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn mcp_returns_200_when_token_required_and_correct() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, Some("secret".into()));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = h.invoke(evt_with_auth(req, "secret")).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn mcp_returns_200_when_no_token_configured() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    /// Auth check runs BEFORE body parsing: malformed body + wrong token → 401 not 400/parse error.
    #[tokio::test]
    async fn mcp_wrong_token_with_malformed_body_returns_401_not_parse_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, Some("secret".into()));
        let malformed_body = "this is definitely not json {{{{";
        let resp = h
            .invoke(evt_with_auth(malformed_body, "wrong-token"))
            .await
            .unwrap();
        assert_eq!(
            resp.status_code, 401,
            "wrong token + malformed body must return 401, not a parse error"
        );
    }

    #[tokio::test]
    async fn tools_list_returns_user_functions_as_tools() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "api");
        assert!(tools[0]["description"].as_str().unwrap().contains("api"));
    }

    /// WebSocket functions have no request/response HTTP route in the Router —
    /// a `tools/call` on one can never succeed. Advertising them in
    /// `tools/list` hands agents a tool that 404s on first use, so they must
    /// be excluded, and calling one by name must be "unknown tool", not a
    /// dispatched 404 envelope.
    #[tokio::test]
    async fn websocket_functions_are_not_advertised_or_callable_as_tools() {
        let s = Arc::new(RizState::new());
        let mut ws_cfg = user_state().config.clone().expect("user_state has config");
        ws_cfg.protocol = crate::config::Protocol::WebSocket;
        s.register(FunctionState::user("chat-ws", ws_cfg, "$default", 0))
            .await;
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);

        // tools/list: only the HTTP function appears.
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools.len(),
            1,
            "WS functions must not be advertised as tools: {body}"
        );
        assert_eq!(tools[0]["name"], "api");

        // tools/call on the WS function: unknown tool, not a 404 envelope.
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chat-ws","arguments":{}}}"#;
        let resp = h.invoke(evt(call)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown function"),
            "calling a WS function must be the same JSON-RPC error as a nonexistent tool: {body}"
        );
    }

    #[tokio::test]
    async fn tools_list_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system(
            "_riz_health",
            vec!["GET /_riz/health".into()],
            "$default",
        ))
        .await;
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "api");
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = "not json";
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_with_missing_router_returns_internal_error() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"api","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32603);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        h.set_router(Arc::new(Router::empty())).await;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32602);
    }

    #[test]
    fn substitute_path_params_replaces_segments() {
        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());
        assert_eq!(
            substitute_path_params("/accounts/{id}", &params),
            "/accounts/42"
        );
    }

    #[test]
    fn substitute_path_params_handles_multiple_segments() {
        let mut params = HashMap::new();
        params.insert("org".to_string(), "anthropic".to_string());
        params.insert("repo".to_string(), "riz".to_string());
        assert_eq!(
            substitute_path_params("/orgs/{org}/repos/{repo}", &params),
            "/orgs/anthropic/repos/riz"
        );
    }

    #[test]
    fn substitute_path_params_passes_through_when_no_pattern() {
        let params = HashMap::new();
        assert_eq!(substitute_path_params("/api", &params), "/api");
    }

    #[test]
    fn substitute_path_params_leaves_unresolved_pattern_intact() {
        // Caller forgot to provide a value — substitution leaves the literal
        // ":id" in place; the Router will 404 on that path.
        let params = HashMap::new();
        assert_eq!(
            substitute_path_params("/accounts/{id}", &params),
            "/accounts/{id}"
        );
    }

    // ─── MCP spec compliance ───────────────────────────────────────────────

    #[tokio::test]
    async fn mcp_spec_2024_11_05_lifecycle() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["serverInfo"]["name"], "riz");
        assert!(
            body["result"]["capabilities"]["tools"].is_object(),
            "tools capability must be advertised"
        );
    }

    #[tokio::test]
    async fn initialize_echoes_supported_client_version() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    }

    #[tokio::test]
    async fn initialize_falls_back_to_default_for_unknown_version() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"9999-99-99"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(
            body["result"]["protocolVersion"],
            SERVER_DEFAULT_PROTOCOL_VERSION
        );
    }

    #[tokio::test]
    async fn ping_returns_empty_object() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["id"], 42);
        assert!(body["result"].is_object());
        assert_eq!(body["result"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn notification_without_id_returns_202_accepted() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(
            resp.status_code, 202,
            "notifications must not produce a body"
        );
        assert!(matches!(resp.body, None | Some(Body::Empty)));
    }

    #[tokio::test]
    async fn notification_with_unknown_method_still_no_response() {
        // Per JSON-RPC 2.0: even errors from notifications produce no response.
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","method":"nonsense/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 202);
    }

    /// A live instance describes itself over MCP: `resources/list` exposes the
    /// function registry (JSON) and a generated llms.txt (the same when-to-use
    /// card `riz scaffold static` writes, but always live).
    #[tokio::test]
    async fn resources_list_exposes_registry_and_llms_txt() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let resources = body["result"]["resources"].as_array().unwrap();
        let uris: Vec<&str> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"riz://registry"), "got: {body}");
        assert!(uris.contains(&"riz://llms.txt"), "got: {body}");
        let reg = resources
            .iter()
            .find(|r| r["uri"] == "riz://registry")
            .unwrap();
        assert_eq!(reg["mimeType"], "application/json", "got: {body}");
        let llms = resources
            .iter()
            .find(|r| r["uri"] == "riz://llms.txt")
            .unwrap();
        assert_eq!(llms["mimeType"], "text/markdown", "got: {body}");
    }

    #[tokio::test]
    async fn resources_read_returns_live_contents() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);

        // The registry resource is the live function registry as JSON.
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"riz://registry"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let content = &body["result"]["contents"][0];
        assert_eq!(content["uri"], "riz://registry", "got: {body}");
        let reg: serde_json::Value =
            serde_json::from_str(content["text"].as_str().unwrap()).expect("registry is JSON");
        assert!(
            reg["functions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["name"] == "api"),
            "registry must list the live function: {reg}"
        );

        // The llms.txt resource describes the same tool surface tools/list
        // advertises.
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"riz://llms.txt"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let text = body["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("### api"),
            "llms.txt must list the tool: {text}"
        );
        assert!(
            text.contains("/_riz/mcp"),
            "must advertise the endpoint: {text}"
        );

        // Unknown uri → the MCP resource-not-found error.
        let req =
            r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"riz://nope"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32002, "got: {body}");
    }

    /// WS functions aren't callable tools, so the llms.txt resource must not
    /// advertise them either (mirrors tools/list).
    #[tokio::test]
    async fn resources_llms_txt_excludes_websocket_functions() {
        let s = Arc::new(RizState::new());
        let mut ws_cfg = user_state().config.clone().expect("user_state has config");
        ws_cfg.protocol = crate::config::Protocol::WebSocket;
        s.register(FunctionState::user("chat-ws", ws_cfg, "$default", 0))
            .await;
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"riz://llms.txt"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let text = body["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("chat-ws"),
            "WS functions must not appear: {text}"
        );
    }

    #[tokio::test]
    async fn initialize_advertises_resources_capability() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert!(
            body["result"]["capabilities"]["resources"].is_object(),
            "resources capability must be advertised: {body}"
        );
    }

    #[tokio::test]
    async fn prompts_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["prompts"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn resources_templates_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/templates/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["resourceTemplates"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn batch_request_returns_array_of_responses() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"[
            {"jsonrpc":"2.0","id":1,"method":"ping"},
            {"jsonrpc":"2.0","id":2,"method":"resources/list"}
        ]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["id"], 2);
        assert!(arr[1]["result"]["resources"].is_array());
    }

    #[tokio::test]
    async fn batch_with_only_notifications_returns_202() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"[
            {"jsonrpc":"2.0","method":"notifications/initialized"},
            {"jsonrpc":"2.0","method":"some/notification"}
        ]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 202);
    }

    #[tokio::test]
    async fn batch_skips_notifications_keeps_request_responses() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"[
            {"jsonrpc":"2.0","method":"notifications/initialized"},
            {"jsonrpc":"2.0","id":7,"method":"ping"}
        ]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the ping request should appear");
        assert_eq!(arr[0]["id"], 7);
    }

    #[tokio::test]
    async fn empty_batch_returns_invalid_request_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"[]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32600);
    }

    // ─── MCP 2025-11-25 spec compliance ─────────────────────────────────────
    //
    // These tests are the regression gate for the spec-version upgrade from
    // 2024-11-05 → 2025-11-25. They lock in: (1) the new default protocol
    // version, (2) structured tool output / outputSchema (added 2025-06-18),
    // (3) Streamable HTTP transport GET handling (2025-03-26+).

    #[tokio::test]
    async fn initialize_with_no_version_defaults_to_2025_11_25() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    }

    #[tokio::test]
    async fn initialize_echoes_2025_06_18_when_requested() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn initialize_echoes_2025_11_25_when_requested() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    }

    #[tokio::test]
    async fn tools_list_advertises_lambda_output_schema() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let tool = &body["result"]["tools"][0];
        assert_eq!(tool["name"], "api");
        let output_schema = &tool["outputSchema"];
        assert!(
            output_schema.is_object(),
            "tools/list must declare outputSchema (MCP 2025-06-18+)"
        );
        assert_eq!(output_schema["type"], "object");
        assert!(
            output_schema["properties"]["statusCode"].is_object(),
            "outputSchema must describe the Lambda response envelope"
        );
        assert_eq!(
            output_schema["required"][0], "statusCode",
            "statusCode is the only universally-required Lambda response field"
        );
    }

    #[tokio::test]
    async fn tools_call_returns_structured_content_with_lambda_envelope() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s, None);
        // Empty router → tools/call hits "unknown tool" path on Router::empty,
        // but for the test we don't need a real handler — wire up a Router
        // that 404s and verify the 404 response is reported as structuredContent.
        h.set_router(Arc::new(Router::empty())).await;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"api","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let result = &body["result"];
        // 2025-06-18+ structuredContent must be present alongside content[].
        assert!(
            result["structuredContent"].is_object(),
            "tools/call must include structuredContent (MCP 2025-06-18+). got: {result}"
        );
        let sc = &result["structuredContent"];
        assert!(
            sc["statusCode"].is_number(),
            "structuredContent must be the Lambda response envelope shape"
        );
        // The text content is still present for older-client back-compat.
        assert!(
            result["content"].is_array() && !result["content"].as_array().unwrap().is_empty(),
            "content array must remain for pre-2025-06-18 clients"
        );
    }

    #[tokio::test]
    async fn get_on_mcp_endpoint_returns_405_with_allow_post() {
        // Streamable HTTP (MCP 2025-03-26+) reserves GET for server-initiated
        // SSE streams. Riz doesn't push, so it must respond cleanly — not 404
        // — so clients can distinguish "transport supported, GET unused" from
        // "wrong endpoint".
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let mut e = make_event_with_body("GET", "/_riz/mcp", "");
        e.http_method = http::Method::GET;
        let resp = h.invoke(e).await.unwrap();
        assert_eq!(resp.status_code, 405);
        let allow = resp
            .headers
            .get("allow")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(allow, "POST", "405 must advertise Allow: POST");
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn initialize_accepts_client_elicitation_capability_silently() {
        // MCP 2025-11-25 adds `elicitation` as a CLIENT capability — clients
        // advertise it so servers know they can call `elicitation/create` for
        // user input. Riz is a server and doesn't drive elicitations, so the
        // capability is informational only. The test guards against a future
        // regression where a strict-parse change would reject unknown
        // capabilities and break newer clients.
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25",
            "capabilities":{
                "elicitation":{},
                "roots":{"listChanged":true},
                "sampling":{}
            },
            "clientInfo":{"name":"newer-client","version":"1.0"}
        }}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        assert!(
            body["result"]["capabilities"]["tools"].is_object(),
            "server must still advertise its own tools capability"
        );
        // Riz must NOT echo back elicitation as a SERVER capability — that
        // would be a lie (we don't initiate elicitations).
        assert!(
            body["result"]["capabilities"]["elicitation"].is_null(),
            "server must not falsely advertise elicitation capability"
        );
    }

    #[tokio::test]
    async fn handler_advertises_both_get_and_post_routes() {
        // McpHandler.routes() must include POST + GET so the Router doesn't
        // 404 the GET path before invoke() can return 405.
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let route_keys: Vec<String> = h
            .routes()
            .iter()
            .map(|r| format!("{} {}", r.method.as_str(), r.path))
            .collect();
        assert!(route_keys.contains(&"POST /_riz/mcp".to_string()));
        assert!(route_keys.contains(&"GET /_riz/mcp".to_string()));
    }
}
