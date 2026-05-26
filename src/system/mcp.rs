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

use async_trait::async_trait;
use http::{header, HeaderMap, HeaderValue, Method};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription, ApiGatewayV2httpResponse, Body,
};
use crate::router::Router;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};
use crate::system::mcp_tool_name;

pub struct McpHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
    router: tokio::sync::RwLock<Option<Arc<Router>>>,
}

impl McpHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Post, path: "/_riz/mcp".into() }],
            riz_state,
            router: tokio::sync::RwLock::new(None),
        }
    }

    /// Called after Router construction (chicken-and-egg: McpHandler is one of
    /// the things the Router holds, and it dispatches reentrantly through it).
    pub async fn set_router(&self, router: Arc<Router>) {
        *self.router.write().await = Some(router);
    }
}

/// MCP protocol versions this server understands. We echo back the client's
/// version if it appears here; otherwise we respond with SERVER_DEFAULT and let
/// the client decide whether to proceed.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26"];
const SERVER_DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: String,
    /// Per JSON-RPC 2.0: absent `id` means this is a notification — no response.
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcOk<T: Serialize> {
    jsonrpc: &'static str,
    id: serde_json::Value,
    result: T,
}

#[derive(Serialize)]
struct JsonRpcErr {
    jsonrpc: &'static str,
    id: serde_json::Value,
    error: JsonRpcErrBody,
}

#[derive(Serialize)]
struct JsonRpcErrBody {
    code: i32,
    message: String,
}

#[derive(Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

#[derive(Serialize)]
struct ToolsListResult {
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct ToolsCallResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Serialize)]
struct ToolContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Deserialize)]
struct ToolsCallParams {
    name: String,
    #[serde(default)]
    arguments: ToolArguments,
}

#[derive(Deserialize, Default)]
struct ToolArguments {
    /// Optional "METHOD /path" selector when the function declares multiple
    /// routes. If omitted, the first declared route is used.
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default, rename = "queryParams")]
    query_params: HashMap<String, String>,
    #[serde(default, rename = "pathParams")]
    path_params: HashMap<String, String>,
    #[serde(default, rename = "isBase64Encoded")]
    is_base64_encoded: bool,
}

#[async_trait]
impl LambdaHandler for McpHandler {
    fn name(&self) -> &str { "POST /_riz/mcp" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, event: ApiGatewayV2httpRequest) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let body = event.body.as_deref().unwrap_or("{}");
        // Parse as raw JSON first to detect batch (array) vs single (object)
        let raw: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return Ok(jsonrpc_error_response(
                serde_json::Value::Null, -32700, &format!("parse error: {e}"),
            )),
        };

        // JSON-RPC 2.0 batch: array of requests. Process each, collect
        // non-notification responses, return a JSON array (or 204 if all
        // were notifications). Empty batch is itself an "Invalid Request".
        if let Some(arr) = raw.as_array() {
            if arr.is_empty() {
                return Ok(jsonrpc_error_response(
                    serde_json::Value::Null, -32600, "empty batch is invalid",
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
            None => Ok(no_content_response()),  // it was a notification
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
                        id, -32600, &format!("invalid request: {e}"),
                    ));
                }
                return None;
            }
        };

        let is_notification = req.id.is_none();
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        let result = match req.method.as_str() {
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

    async fn initialize(&self, params: serde_json::Value) -> Result<serde_json::Value, JsonRpcError> {
        // Best-effort client protocol version negotiation.
        let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
        let chosen = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
            requested
        } else {
            SERVER_DEFAULT_PROTOCOL_VERSION
        };
        Ok(serde_json::json!({
            "protocolVersion": chosen,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "riz",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }))
    }

    async fn tools_list_value(&self) -> Result<serde_json::Value, JsonRpcError> {
        let functions = self.riz_state.functions.read().await;
        let mut tools = Vec::new();
        for (_, f) in functions.iter() {
            if !matches!(f.kind, FunctionKind::User) { continue; }
            // MCP tool name = function name directly (no transformation needed
            // now that we're function-centric).
            let name = f.name.clone();
            let description = match &f.config {
                Some(c) => format!(
                    "Invoke function `{}` ({} runtime). Routes: [{}]",
                    f.name, c.runtime.as_str(), f.routes.join(", "),
                ),
                None => format!("Invoke {}", f.name),
            };
            tools.push(Tool {
                name,
                description,
                input_schema: generic_envelope_schema(),
            });
        }
        let value = serde_json::to_value(ToolsListResult { tools })
            .map_err(|e| JsonRpcError { code: -32603, message: e.to_string() })?;
        Ok(value)
    }

    async fn tools_call_value(&self, params: serde_json::Value) -> Result<serde_json::Value, JsonRpcError> {
        let parsed: ToolsCallParams = serde_json::from_value(params)
            .map_err(|e| JsonRpcError {
                code: -32602,
                message: format!("invalid params: {e}"),
            })?;

        // Tool name == function name. Look up the function and pick a route
        // to dispatch to: use the caller-supplied `route` arg if present,
        // otherwise default to the function's first declared route.
        let (function_name, method, path) = {
            let functions = self.riz_state.functions.read().await;
            let f = functions.get(&parsed.name)
                .filter(|f| matches!(f.kind, FunctionKind::User))
                .ok_or_else(|| JsonRpcError {
                    code: -32602,
                    message: format!("unknown function: {}", parsed.name),
                })?
                .clone();
            // Routes are stored as "METHOD /path" strings on FunctionState.
            // Pick the requested one (if the caller passed `route`), else the first.
            let requested = parsed.arguments.route.as_deref();
            let chosen = match requested {
                Some(want) => f.routes.iter()
                    .find(|r| r.as_str() == want)
                    .ok_or_else(|| JsonRpcError {
                        code: -32602,
                        message: format!("route '{want}' not declared by function '{}'", f.name),
                    })?
                    .clone(),
                None => f.routes.first()
                    .ok_or_else(|| JsonRpcError {
                        code: -32603,
                        message: format!("function '{}' has no routes", f.name),
                    })?
                    .clone(),
            };
            let (m, p) = chosen.split_once(' ').ok_or_else(|| JsonRpcError {
                code: -32603,
                message: format!("malformed route entry: {chosen}"),
            })?;
            (f.name.clone(), m.to_string(), p.to_string())
        };
        let route_key = format!("{} {}", method, path);

        // Build an AWS v2 event. If the matched route is a pattern like
        // `/accounts/{id}`, substitute `{id}` with the caller-supplied
        // pathParams.id; the Router re-extracts params during dispatch.
        let args = parsed.arguments;
        let concrete_path = substitute_path_params(&path, &args.path_params);
        let raw_qs = args.query_params.iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let method_typed = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
        let mut hmap = HeaderMap::new();
        for (k, v) in args.headers.iter() {
            if let (Ok(name), Ok(value)) = (
                http::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                hmap.insert(name, value);
            }
        }
        let qmap: aws_lambda_events::query_map::QueryMap = args.query_params.clone().into();
        let mut ctx = ApiGatewayV2httpRequestContext::default();
        ctx.route_key = Some(route_key.clone());
        ctx.account_id = Some("riz".into());
        ctx.stage = Some("$default".into());
        ctx.request_id = Some(uuid::Uuid::new_v4().to_string());
        ctx.time_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        ctx.http = ApiGatewayV2httpRequestContextHttpDescription {
            method: method_typed.clone(),
            path: Some(concrete_path.clone()),
            protocol: Some("HTTP/1.1".into()),
            source_ip: Some("127.0.0.1".into()),
            user_agent: Some("riz-mcp".into()),
        };
        let event = ApiGatewayV2httpRequest {
            version: Some("2.0".into()),
            route_key: Some(route_key.clone()),
            raw_path: Some(concrete_path.clone()),
            raw_query_string: Some(raw_qs),
            cookies: None,
            headers: hmap,
            query_string_parameters: qmap,
            path_parameters: Default::default(),
            request_context: ctx,
            stage_variables: Default::default(),
            body: args.body,
            is_base64_encoded: args.is_base64_encoded,
            kind: None,
            method_arn: None,
            http_method: method_typed,
            identity_source: None,
            authorization_token: None,
            resource: None,
        };

        // Reentrant dispatch through the same Router.
        let router = self.router.read().await;
        let router = router.as_ref()
            .cloned()
            .ok_or_else(|| JsonRpcError {
                code: -32603,
                message: "router not initialized".into(),
            })?;
        let inner = match router.dispatch(event).await {
            Ok(outcome) => outcome.response,
            Err(e) => e.to_response(),
        };

        let is_error = inner.status_code >= 400;
        let inner_json = serde_json::to_string(&inner)
            .map_err(|e| JsonRpcError { code: -32603, message: e.to_string() })?;
        let result = ToolsCallResult {
            content: vec![ToolContent { kind: "text", text: inner_json }],
            is_error,
        };
        let value = serde_json::to_value(result)
            .map_err(|e| JsonRpcError { code: -32603, message: e.to_string() })?;
        Ok(value)
    }
}

/// Internal error type for the new dispatcher — converted to JSON-RPC error
/// shape at the response boundary.
struct JsonRpcError {
    code: i32,
    message: String,
}

fn generic_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "route": {"type": "string", "description": "Optional \"METHOD /path\" selector when the function declares multiple routes. Omit to use the first declared route."},
            "body": {"type": "string", "description": "Request body. Set isBase64Encoded:true for binary."},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}},
            "queryParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "pathParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "isBase64Encoded": {"type": "boolean", "default": false}
        }
    })
}

/// Substitute `{name}` and `{name+}` segments in `pattern` with values from
/// `params`. Segments without a matching param key are left as-is (caller
/// error — the Router's match will then reject the request as a 404).
fn substitute_path_params(pattern: &str, params: &HashMap<String, String>) -> String {
    if !pattern.contains('{') {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len());
    let mut first = true;
    for seg in pattern.trim_start_matches('/').split('/') {
        if !first { out.push('/'); }
        first = false;
        // `{name+}` greedy
        if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix("+}")) {
            if let Some(v) = params.get(inner) {
                out.push_str(v);
            } else {
                out.push_str(seg);
            }
            continue;
        }
        // `{name}` single
        if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if let Some(v) = params.get(inner) {
                out.push_str(v);
            } else {
                out.push_str(seg);
            }
        } else {
            out.push_str(seg);
        }
    }
    if pattern.starts_with('/') {
        let mut prefixed = String::with_capacity(out.len() + 1);
        prefixed.push('/');
        prefixed.push_str(&out);
        prefixed
    } else {
        out
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

/// Wrap any JSON value in a 200 response with content-type application/json.
fn json_response(value: serde_json::Value) -> ApiGatewayV2httpResponse {
    let json = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    ApiGatewayV2httpResponse {
        status_code: 200,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(json)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// 204 No Content — used when the entire request was notifications.
fn no_content_response() -> ApiGatewayV2httpResponse {
    ApiGatewayV2httpResponse {
        status_code: 204,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        body: None,
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// Build a JSON-RPC error envelope around a single id, return as a full HTTP
/// response. Used at the top of `invoke` for parse/batch-shape failures.
fn jsonrpc_error_response(id: serde_json::Value, code: i32, message: &str) -> ApiGatewayV2httpResponse {
    json_response(jsonrpc_error_value(id, code, message))
}

/// Just the JSON-RPC error envelope as a JSON value — used inside batch
/// processing where we collect responses into an array.
fn jsonrpc_error_value(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;
    use crate::test_helpers::make_event_with_body;

    fn evt(body: &str) -> ApiGatewayV2httpRequest {
        make_event_with_body("POST", "/_riz/mcp", body)
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
        };
        FunctionState::user("api", c)
    }

    #[tokio::test]
    async fn tools_list_returns_user_functions_as_tools() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
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
        s.register(FunctionState::system("_riz_health", vec!["GET /_riz/health".into()])).await;
        s.register(user_state()).await;
        let h = McpHandler::new(s);
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
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = "not json";
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_with_missing_router_returns_internal_error() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"api","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32603);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
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
        assert_eq!(substitute_path_params("/accounts/{id}", &params), "/accounts/42");
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
        assert_eq!(substitute_path_params("/accounts/{id}", &params), "/accounts/{id}");
    }

    // ─── MCP spec compliance ───────────────────────────────────────────────

    #[tokio::test]
    async fn mcp_spec_2024_11_05_lifecycle() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["serverInfo"]["name"], "riz");
        assert!(body["result"]["capabilities"]["tools"].is_object(),
            "tools capability must be advertised");
    }

    #[tokio::test]
    async fn initialize_echoes_supported_client_version() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    }

    #[tokio::test]
    async fn initialize_falls_back_to_default_for_unknown_version() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"9999-99-99"}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["protocolVersion"], SERVER_DEFAULT_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn ping_returns_empty_object() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
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
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 204, "notifications must not produce a body");
        assert!(matches!(resp.body, None | Some(Body::Empty)));
    }

    #[tokio::test]
    async fn notification_with_unknown_method_still_no_response() {
        // Per JSON-RPC 2.0: even errors from notifications produce no response.
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","method":"nonsense/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        assert_eq!(resp.status_code, 204);
    }

    #[tokio::test]
    async fn resources_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["resources"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn prompts_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["prompts"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn resources_templates_list_returns_empty_array() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/templates/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["result"]["resourceTemplates"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn batch_request_returns_array_of_responses() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
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
        let h = McpHandler::new(s);
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
        let h = McpHandler::new(s);
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
        let h = McpHandler::new(s);
        let req = r#"[]"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["error"]["code"], -32600);
    }
}
