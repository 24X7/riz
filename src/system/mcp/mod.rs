//! /_riz/mcp handler — full MCP-spec-compliant JSON-RPC 2.0 server.
//!
//! Supports the lifecycle (`initialize`, `notifications/initialized`, `ping`),
//! tools (`tools/list`, `tools/call`), and empty implementations of
//! `resources/list` + `prompts/list` so probing clients don't error.
//!
//! Each user function in the riz.toml becomes one MCP tool. tools/call
//! assembles a ApiGatewayV2httpRequest from the supplied arguments and dispatches it
//! through the Router — so any function becomes MCP-callable with no changes
//! to the function's own code.
//!
//! Transport: stateless HTTP. One JSON-RPC message (or a batch array) per
//! POST. Notifications (requests without `id`) get a 204 No Content. Batches
//! return a 200 with an array of responses (notifications inside a batch
//! contribute nothing).
//!
//! Protocol version: advertises "2024-11-05" — the widely-supported baseline.
//! On `initialize`, echoes the version requested by the client if recognized;
//! otherwise responds with the baseline (client may choose to disconnect).

mod encoding;
mod protocol;
mod tools;

use crate::auth::bearer::validate_bearer;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::router::Router;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::RizState;
use async_trait::async_trait;
use std::sync::Arc;

use crate::runtime::response::json_response as http_json_response;
use encoding::{json_response, jsonrpc_error_response, jsonrpc_error_value, no_content_response};
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
            routes: vec![RouteEntry {
                method: RouteMethod::Post,
                path: "/_riz/mcp".into(),
            }],
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
        // non-notification responses, return a JSON array (or 204 if all
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
                no_content_response()
            } else {
                json_response(serde_json::Value::Array(out))
            });
        }

        // Single request (object).
        match self.process_one(&raw).await {
            Some(resp) => Ok(json_response(resp)),
            None => Ok(no_content_response()), // it was a notification
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

            // Resources / Prompts — Riz doesn't expose these, but return
            // empty lists so probing clients don't choke on -32601.
            "resources/list" => Ok(serde_json::json!({ "resources": [] })),
            "resources/templates/list" => Ok(serde_json::json!({ "resourceTemplates": [] })),
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
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
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
    async fn notification_without_id_returns_204_no_content() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(
            resp.status_code, 204,
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
        assert_eq!(resp.status_code, 204);
    }

    #[tokio::test]
    async fn resources_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["resources"], serde_json::json!([]));
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
    async fn batch_with_only_notifications_returns_204() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s, None);
        let req = r#"[
            {"jsonrpc":"2.0","method":"notifications/initialized"},
            {"jsonrpc":"2.0","method":"some/notification"}
        ]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 204);
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
}
