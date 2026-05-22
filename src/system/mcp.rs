//! /_riz/mcp handler — JSON-RPC 2.0 implementing MCP tools/list + tools/call.
//!
//! For tools/call, the handler assembles a GatewayRequest from the supplied
//! arguments (using a generic envelope schema) and dispatches it back through
//! the Router — so any user function becomes an MCP-callable tool with no
//! changes to the function's own code.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
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

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: String,
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

    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let body = event.body.as_deref().unwrap_or("{}");
        let req: JsonRpcRequest = match serde_json::from_str(body) {
            Ok(r) => r,
            Err(e) => return Ok(jsonrpc_error(serde_json::Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        match req.method.as_str() {
            "tools/list" => self.tools_list(id).await,
            "tools/call" => self.tools_call(id, req.params).await,
            other => Ok(jsonrpc_error(id, -32601, &format!("method not found: {other}"))),
        }
    }
}

impl McpHandler {
    async fn tools_list(&self, id: serde_json::Value) -> Result<GatewayResponse, HandlerError> {
        let functions = self.riz_state.functions.read().await;
        let mut tools = Vec::new();
        for (_, f) in functions.iter() {
            if !matches!(f.kind, FunctionKind::User) { continue; }
            let name = mcp_tool_name(&f.route_key);
            let description = match &f.route {
                Some(r) => format!("Invoke {} ({} runtime)", f.route_key, r.runtime.as_str()),
                None => format!("Invoke {}", f.route_key),
            };
            tools.push(Tool {
                name,
                description,
                input_schema: generic_envelope_schema(),
            });
        }
        let result = ToolsListResult { tools };
        ok_response(id, result)
    }

    async fn tools_call(&self, id: serde_json::Value, params: serde_json::Value) -> Result<GatewayResponse, HandlerError> {
        let parsed: ToolsCallParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => return Ok(jsonrpc_error(id, -32602, &format!("invalid params: {e}"))),
        };

        // Look up the matching route by tool-name derivation.
        let matched: Option<(String, String, String)> = {
            let functions = self.riz_state.functions.read().await;
            let mut found = None;
            for (route_key, f) in functions.iter() {
                if !matches!(f.kind, FunctionKind::User) { continue; }
                if mcp_tool_name(route_key) == parsed.name {
                    if let Some((m, p)) = route_key.split_once(' ') {
                        found = Some((route_key.clone(), m.to_string(), p.to_string()));
                        break;
                    }
                }
            }
            found
        };

        let (route_key, method, path) = match matched {
            Some(m) => m,
            None => return Ok(jsonrpc_error(id, -32602, &format!("unknown tool: {}", parsed.name))),
        };

        // Build a GatewayRequest from the tool arguments. If the matched
        // route is a pattern like `/accounts/:id`, substitute `:id` with the
        // caller-supplied pathParams.id so the Router can re-extract the
        // params during dispatch (the canonical extraction path).
        let args = parsed.arguments;
        let concrete_path = substitute_path_params(&path, &args.path_params);
        let raw_qs = args.query_params.iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let event = GatewayRequest {
            version: "2.0".into(),
            route_key: route_key.clone(),
            raw_path: concrete_path.clone(),
            raw_query_string: raw_qs,
            headers: args.headers,
            request_context: RequestContext {
                http: HttpContext {
                    method: method.clone(),
                    path: concrete_path,
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: uuid::Uuid::new_v4().to_string(),
                time_epoch: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            },
            // path_parameters is left None — the Router will populate it from
            // its own pattern match on concrete_path.
            path_parameters: None,
            body: args.body,
            is_base64_encoded: args.is_base64_encoded,
        };

        // Reentrant dispatch through the same Router that called this handler.
        let router = self.router.read().await;
        let router = match router.as_ref() {
            Some(r) => r.clone(),
            None => return Ok(jsonrpc_error(id, -32603, "router not initialized")),
        };
        let inner = match router.dispatch(event).await {
            Ok(outcome) => outcome.response,
            Err(e) => e.to_response(),
        };

        let is_error = inner.status_code >= 400;
        let inner_json = serde_json::to_string(&inner)
            .map_err(|e| HandlerError::Internal(e.to_string()))?;
        let result = ToolsCallResult {
            content: vec![ToolContent { kind: "text", text: inner_json }],
            is_error,
        };
        ok_response(id, result)
    }
}

fn generic_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "body": {"type": "string", "description": "Request body. Set isBase64Encoded:true for binary."},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}},
            "queryParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "pathParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "isBase64Encoded": {"type": "boolean", "default": false}
        }
    })
}

/// Substitute `:name` segments in `pattern` with values from `params`.
/// Segments without a matching param key are left as-is (caller error,
/// the Router's match will then reject the request as a 404).
fn substitute_path_params(pattern: &str, params: &HashMap<String, String>) -> String {
    if !pattern.contains(':') {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len());
    let mut first = true;
    for seg in pattern.trim_start_matches('/').split('/') {
        if !first { out.push('/'); }
        first = false;
        if let Some(name) = seg.strip_prefix(':') {
            if let Some(v) = params.get(name) {
                out.push_str(v);
            } else {
                out.push_str(seg);  // unresolved param — Router will 404
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

fn ok_response<T: Serialize>(id: serde_json::Value, result: T) -> Result<GatewayResponse, HandlerError> {
    let body = JsonRpcOk { jsonrpc: "2.0", id, result };
    let json = serde_json::to_string(&body)
        .map_err(|e| HandlerError::Internal(e.to_string()))?;
    let mut headers = HashMap::new();
    headers.insert("content-type".into(), "application/json".into());
    Ok(GatewayResponse {
        status_code: 200,
        headers: Some(headers),
        body: Some(json),
        is_base64_encoded: None,
    })
}

fn jsonrpc_error(id: serde_json::Value, code: i32, message: &str) -> GatewayResponse {
    let body = JsonRpcErr {
        jsonrpc: "2.0",
        id,
        error: JsonRpcErrBody { code, message: message.to_string() },
    };
    let json = serde_json::to_string(&body).unwrap_or_else(|_| String::from("{}"));
    let mut headers = HashMap::new();
    headers.insert("content-type".into(), "application/json".into());
    GatewayResponse {
        status_code: 200,  // JSON-RPC errors travel as 200 with error body
        headers: Some(headers),
        body: Some(json),
        is_base64_encoded: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;

    fn evt(body: &str) -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "POST /_riz/mcp".into(),
            raw_path: "/_riz/mcp".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "POST".into(),
                    path: "/_riz/mcp".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: Some(body.to_string()),
            is_base64_encoded: false,
        }
    }

    fn user_state() -> FunctionState {
        let r = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn tools_list_returns_user_functions_as_tools() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "GET_api");
        assert!(tools[0]["description"].as_str().unwrap().contains("GET /api"));
    }

    #[tokio::test]
    async fn tools_list_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "GET_api");
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method"}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        let req = "not json";
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_with_missing_router_returns_internal_error() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = McpHandler::new(s);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"GET_api","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32603);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_returns_jsonrpc_error() {
        let s = Arc::new(RizState::new());
        let h = McpHandler::new(s);
        h.set_router(Arc::new(Router::empty())).await;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"GET_nope","arguments":{}}}"#;
        let resp = h.invoke(evt(req)).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], -32602);
    }

    #[test]
    fn substitute_path_params_replaces_segments() {
        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());
        assert_eq!(substitute_path_params("/accounts/:id", &params), "/accounts/42");
    }

    #[test]
    fn substitute_path_params_handles_multiple_segments() {
        let mut params = HashMap::new();
        params.insert("org".to_string(), "anthropic".to_string());
        params.insert("repo".to_string(), "riz".to_string());
        assert_eq!(
            substitute_path_params("/orgs/:org/repos/:repo", &params),
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
        assert_eq!(substitute_path_params("/accounts/:id", &params), "/accounts/:id");
    }
}
