//! MCP tools: initialize, tools/list, tools/call implementations.
//! These are impl blocks on McpHandler defined in mod.rs.

use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
};
use crate::state::FunctionKind;
use http::{HeaderMap, HeaderValue, Method};

use super::encoding::{lambda_response_envelope_schema, substitute_path_params, urlencode};
use super::protocol::{
    JsonRpcError, Tool, ToolContent, ToolsCallParams, ToolsCallResult, ToolsListResult,
};
use super::schema::{path_param_names, tool_input_schema};
use super::McpHandler;

impl McpHandler {
    pub(super) async fn initialize(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, JsonRpcError> {
        use super::protocol::{SERVER_DEFAULT_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};
        // Best-effort client protocol version negotiation.
        let requested = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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

    pub(super) async fn tools_list_value(&self) -> Result<serde_json::Value, JsonRpcError> {
        let functions = self.riz_state.functions.read().await;
        let mut tools = Vec::new();
        for (_, f) in functions.iter() {
            if !matches!(f.kind, FunctionKind::User) {
                continue;
            }
            // WebSocket functions have no request/response HTTP route in the
            // Router — a tools/call on one can never succeed, so advertising
            // them would hand agents a tool that 404s on first use.
            if is_websocket(f) {
                continue;
            }
            // MCP tool name = function name directly (no transformation needed
            // now that we're function-centric).
            let name = f.name.clone();
            let mcp_cfg = f.config.as_ref().and_then(|c| c.mcp.as_ref());
            // [function.X.mcp] description wins; otherwise the generated one.
            let description = match mcp_cfg.and_then(|m| m.description.clone()) {
                Some(d) => d,
                None => match &f.config {
                    Some(c) => format!(
                        "Invoke function `{}` ({} runtime). Routes: [{}]",
                        f.name,
                        c.runtime.as_str(),
                        f.routes.join(", "),
                    ),
                    None => format!("Invoke {}", f.name),
                },
            };
            tools.push(Tool {
                name,
                description,
                input_schema: tool_input_schema(&f.routes, mcp_cfg),
                output_schema: Some(lambda_response_envelope_schema()),
            });
        }
        let value = serde_json::to_value(ToolsListResult { tools }).map_err(|e| JsonRpcError {
            code: -32603,
            message: e.to_string(),
        })?;
        Ok(value)
    }

    pub(super) async fn tools_call_value(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, JsonRpcError> {
        let parsed: ToolsCallParams = serde_json::from_value(params).map_err(|e| JsonRpcError {
            code: -32602,
            message: format!("invalid params: {e}"),
        })?;

        // Tool name == function name. Look up the function and pick a route
        // to dispatch to: use the caller-supplied `route` arg if present,
        // otherwise default to the function's first declared route.
        let (_function_name, method, path, mcp_query_specs) = {
            let functions = self.riz_state.functions.read().await;
            let f = functions
                .get(&parsed.name)
                // WS functions aren't advertised (no HTTP route to dispatch) —
                // calling one by name is the same error as a nonexistent tool.
                .filter(|f| matches!(f.kind, FunctionKind::User) && !is_websocket(f))
                .ok_or_else(|| JsonRpcError {
                    code: -32602,
                    message: format!("unknown function: {}", parsed.name),
                })?
                .clone();
            // Routes are stored as "METHOD /path" strings on FunctionState.
            // Pick the requested one (if the caller passed `route`), else the first.
            let requested = parsed.arguments.route.as_deref();
            let chosen = match requested {
                Some(want) => f
                    .routes
                    .iter()
                    .find(|r| r.as_str() == want)
                    .ok_or_else(|| JsonRpcError {
                        code: -32602,
                        message: format!("route '{want}' not declared by function '{}'", f.name),
                    })?
                    .clone(),
                None => f
                    .routes
                    .first()
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
            let specs = f
                .config
                .as_ref()
                .and_then(|c| c.mcp.as_ref())
                .map(|mcp| mcp.query.clone())
                .unwrap_or_default();
            (f.name.clone(), m.to_string(), p.to_string(), specs)
        };
        let route_key = format!("{} {}", method, path);

        // ── Typed-schema validation (v1 roadmap #13) ─────────────────────
        // Once a route is chosen, every {param} in its template is required —
        // an unsubstituted segment can only dispatch to a 404. Reject up
        // front with the param named so the agent can self-correct.
        let missing: Vec<String> = path_param_names(&path)
            .into_iter()
            .filter(|p| !parsed.arguments.path_params.contains_key(p))
            .collect();
        if !missing.is_empty() {
            return Err(JsonRpcError {
                code: -32602,
                message: format!(
                    "missing required path parameter(s) for route '{route_key}': {}",
                    missing.join(", ")
                ),
            });
        }
        // Declared query params: required ones must be present; provided
        // values must parse as their declared scalar type. (Values arrive as
        // wire strings — scalar JSON args were already coerced at
        // deserialization.)
        for (pname, spec) in &mcp_query_specs {
            match parsed.arguments.query_params.get(pname) {
                None if spec.required => {
                    return Err(JsonRpcError {
                        code: -32602,
                        message: format!("missing required query parameter '{pname}'"),
                    });
                }
                None => {}
                Some(value) => {
                    let ok = match spec.kind.as_str() {
                        "integer" => value.parse::<i64>().is_ok(),
                        "number" => value.parse::<f64>().is_ok(),
                        "boolean" => matches!(value.as_str(), "true" | "false"),
                        _ => true, // string — anything goes
                    };
                    if !ok {
                        return Err(JsonRpcError {
                            code: -32602,
                            message: format!(
                                "query parameter '{pname}' must be a {} (got '{value}')",
                                spec.kind
                            ),
                        });
                    }
                }
            }
        }

        // Build an AWS v2 event. If the matched route is a pattern like
        // `/accounts/{id}`, substitute `{id}` with the caller-supplied
        // pathParams.id; the Router re-extracts params during dispatch.
        let args = parsed.arguments;
        let concrete_path = substitute_path_params(&path, &args.path_params);
        let raw_qs = args
            .query_params
            .iter()
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
        let ctx = ApiGatewayV2httpRequestContext {
            route_key: Some(route_key.clone()),
            account_id: Some("riz".into()),
            stage: Some("$default".into()),
            request_id: Some(uuid::Uuid::new_v4().to_string()),
            time_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            http: ApiGatewayV2httpRequestContextHttpDescription {
                method: method_typed.clone(),
                path: Some(concrete_path.clone()),
                protocol: Some("HTTP/1.1".into()),
                source_ip: Some("127.0.0.1".into()),
                user_agent: Some("riz-mcp".into()),
            },
            ..Default::default()
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
        let router = router.as_ref().cloned().ok_or_else(|| JsonRpcError {
            code: -32603,
            message: "router not initialized".into(),
        })?;
        let inner = match router.dispatch(event).await {
            Ok(outcome) => outcome.response,
            Err(e) => e.to_response(),
        };

        let is_error = inner.status_code >= 400;
        let inner_value = serde_json::to_value(&inner).map_err(|e| JsonRpcError {
            code: -32603,
            message: e.to_string(),
        })?;
        let inner_text = serde_json::to_string(&inner_value).map_err(|e| JsonRpcError {
            code: -32603,
            message: e.to_string(),
        })?;
        let result = ToolsCallResult {
            content: vec![ToolContent {
                kind: "text",
                text: inner_text,
            }],
            // 2025-06-18+ clients prefer this typed shape over re-parsing
            // content[0].text; older clients ignore the unknown field.
            structured_content: Some(inner_value),
            is_error,
        };
        let value = serde_json::to_value(result).map_err(|e| JsonRpcError {
            code: -32603,
            message: e.to_string(),
        })?;
        Ok(value)
    }
}

/// True when the function speaks the WebSocket lifecycle ($connect/$default/
/// $disconnect) — those mount as upgrade routes, not request/response HTTP
/// routes, so the MCP tool surface cannot dispatch to them.
fn is_websocket(f: &crate::state::FunctionState) -> bool {
    f.config
        .as_ref()
        .is_some_and(|c| matches!(c.protocol, crate::config::Protocol::WebSocket))
}
