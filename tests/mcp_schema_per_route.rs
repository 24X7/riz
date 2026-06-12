//! Per-route typed MCP tool schemas (v1 roadmap #13).
//!
//! A function with `routes = [GET /accounts/{id}]` must expose a tool whose
//! `inputSchema` types `pathParams.id` from the path template, and query
//! params declared in `[function.X.mcp.query]` become typed input fields.
//! tools/call validates required path params and declared query types, and
//! coerces scalar JSON arguments (numbers/bools) to the wire strings the
//! Lambda event carries.

use std::net::SocketAddr;
use std::sync::Arc;

use riz::config::{Config, FunctionConfig, McpParamSpec, McpToolConfig, RouteSpec, RuntimeKind};

/// Base config for a synthetic user function (never actually spawned —
/// tools/list and tools/call *validation* never reach a process).
fn base_cfg(routes: Vec<(&str, &str)>) -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from("./echo.ts"),
        timeout_ms: 5000,
        integration_timeout_ms: 30000,
        stage_variables: Default::default(),
        cache_ttl_secs: None,
        concurrency: 1,
        routes: routes
            .into_iter()
            .map(|(m, p)| RouteSpec {
                path: p.into(),
                method: m.into(),
            })
            .collect(),
        cors: None,
        authorizer: None,
        memory_mb: None,
        cpu_time_secs: None,
        allowed_paths: None,
        mcp: None,
    }
}

async fn make_state(funcs: Vec<(&str, FunctionConfig)>) -> Arc<riz::state::AppState> {
    let config = Config::default();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_mcp",
            vec!["POST /_riz/mcp".into()],
            "$default",
        ))
        .await;
    for (name, cfg) in funcs {
        riz_state
            .register(riz::state::FunctionState::user(name, cfg, "$default", 0))
            .await;
    }

    let mcp = Arc::new(riz::system::mcp::McpHandler::new(riz_state.clone(), None));
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> =
        vec![mcp.clone() as Arc<dyn riz::runtime::LambdaHandler>];
    let router_arc = Arc::new(riz::router::Router::new(handlers.clone()));
    mcp.set_router(router_arc).await;

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(handlers)),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
    })
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn rpc(addr: SocketAddr, req: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn tool_schema(addr: SocketAddr, name: &str) -> serde_json::Value {
    let body = rpc(
        addr,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let tools = body["result"]["tools"].as_array().unwrap().clone();
    let tool = tools
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("tool '{name}' not listed in {tools:?}"));
    tool["inputSchema"].clone()
}

// ───────────────────────────── tools/list shapes ────────────────────────────

#[tokio::test]
async fn tools_list_types_path_params_from_route_template() {
    let state = make_state(vec![(
        "accounts",
        base_cfg(vec![("GET", "/accounts/{id}")]),
    )])
    .await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "accounts").await;

    let id = &schema["properties"]["pathParams"]["properties"]["id"];
    assert_eq!(id["type"], "string", "typed path param: {schema}");
    // The path param is required inside pathParams, and pathParams itself is
    // required at the top level — Claude can't omit it.
    let pp_required = schema["properties"]["pathParams"]["required"]
        .as_array()
        .unwrap();
    assert!(pp_required.iter().any(|v| v == "id"), "{schema}");
    let top_required = schema["required"].as_array().unwrap();
    assert!(top_required.iter().any(|v| v == "pathParams"), "{schema}");
}

#[tokio::test]
async fn tools_list_types_greedy_path_param() {
    let state = make_state(vec![("files", base_cfg(vec![("GET", "/files/{key+}")]))]).await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "files").await;
    assert_eq!(
        schema["properties"]["pathParams"]["properties"]["key"]["type"],
        "string",
        "{schema}"
    );
}

#[tokio::test]
async fn tools_list_types_declared_query_params() {
    let mut cfg = base_cfg(vec![("GET", "/search")]);
    let mut query = indexmap::IndexMap::new();
    query.insert(
        "limit".to_string(),
        McpParamSpec {
            kind: "integer".into(),
            description: Some("Max results".into()),
            required: true,
        },
    );
    query.insert(
        "verbose".to_string(),
        McpParamSpec {
            kind: "boolean".into(),
            description: None,
            required: false,
        },
    );
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query,
        body: None,
    });
    let state = make_state(vec![("search", cfg)]).await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "search").await;

    let qp = &schema["properties"]["queryParams"];
    assert_eq!(qp["properties"]["limit"]["type"], "integer", "{schema}");
    assert_eq!(qp["properties"]["limit"]["description"], "Max results");
    assert_eq!(qp["properties"]["verbose"]["type"], "boolean");
    let required = qp["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "limit"), "{schema}");
    assert!(!required.iter().any(|v| v == "verbose"), "{schema}");
}

#[tokio::test]
async fn tools_list_route_enum_for_multi_route_function() {
    let state = make_state(vec![(
        "orders",
        base_cfg(vec![("GET", "/orders/{id}"), ("POST", "/orders")]),
    )])
    .await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "orders").await;
    let route_enum = schema["properties"]["route"]["enum"].as_array().unwrap();
    assert!(route_enum.iter().any(|v| v == "GET /orders/{id}"), "{schema}");
    assert!(route_enum.iter().any(|v| v == "POST /orders"), "{schema}");
}

#[tokio::test]
async fn tools_list_uses_mcp_description_override() {
    let mut cfg = base_cfg(vec![("GET", "/accounts/{id}")]);
    cfg.mcp = Some(McpToolConfig {
        description: Some("Look up an account by id.".into()),
        query: Default::default(),
        body: None,
    });
    let state = make_state(vec![("accounts", cfg)]).await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let tool = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "accounts")
        .unwrap()
        .clone();
    assert_eq!(tool["description"], "Look up an account by id.");
}

#[tokio::test]
async fn tools_list_uses_declared_body_schema() {
    let mut cfg = base_cfg(vec![("POST", "/orders")]);
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query: Default::default(),
        body: Some(serde_json::json!({
            "type": "object",
            "properties": {"sku": {"type": "string"}, "qty": {"type": "integer"}},
            "required": ["sku", "qty"]
        })),
    });
    let state = make_state(vec![("orders", cfg)]).await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "orders").await;
    assert_eq!(
        schema["properties"]["body"]["properties"]["sku"]["type"],
        "string",
        "{schema}"
    );
}

#[tokio::test]
async fn tools_list_keeps_generic_schema_when_nothing_declared() {
    // Back-compat: no path params, no [function.X.mcp] block — the generic
    // envelope shape stays (free-form string maps).
    let state = make_state(vec![("echo", base_cfg(vec![("GET", "/echo")]))]).await;
    let addr = serve(state).await;
    let schema = tool_schema(addr, "echo").await;
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["body"].is_object(), "{schema}");
    assert!(
        schema["properties"]["queryParams"]["additionalProperties"].is_object(),
        "{schema}"
    );
    // No required constraints invented for a schema-less function.
    assert!(schema.get("required").is_none(), "{schema}");
}

// ───────────────────────────── tools/call validation ────────────────────────

#[tokio::test]
async fn tools_call_missing_required_path_param_is_invalid_params() {
    let state = make_state(vec![(
        "accounts",
        base_cfg(vec![("GET", "/accounts/{id}")]),
    )])
    .await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": {"name": "accounts", "arguments": {}}
        }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602, "{body}");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("id"), "message should name the param: {msg}");
}

#[tokio::test]
async fn tools_call_query_type_mismatch_is_invalid_params() {
    let mut cfg = base_cfg(vec![("GET", "/search")]);
    let mut query = indexmap::IndexMap::new();
    query.insert(
        "limit".to_string(),
        McpParamSpec {
            kind: "integer".into(),
            description: None,
            required: true,
        },
    );
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query,
        body: None,
    });
    let state = make_state(vec![("search", cfg)]).await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": {"name": "search", "arguments": {"queryParams": {"limit": "abc"}}}
        }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602, "{body}");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("limit"), "{msg}");
    assert!(msg.contains("integer"), "{msg}");
}

#[tokio::test]
async fn tools_call_missing_required_query_param_is_invalid_params() {
    let mut cfg = base_cfg(vec![("GET", "/search")]);
    let mut query = indexmap::IndexMap::new();
    query.insert(
        "limit".to_string(),
        McpParamSpec {
            kind: "integer".into(),
            description: None,
            required: true,
        },
    );
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query,
        body: None,
    });
    let state = make_state(vec![("search", cfg)]).await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": {"name": "search", "arguments": {}}
        }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602, "{body}");
}

#[tokio::test]
async fn tools_call_coerces_scalar_json_args() {
    // A client following the typed schema sends limit as a JSON number and
    // the path id as a number — both must coerce to wire strings, not fail
    // deserialization. Dispatch then proceeds (and fails at the missing
    // process, NOT at -32602) — proving validation passed.
    let mut cfg = base_cfg(vec![("GET", "/accounts/{id}")]);
    let mut query = indexmap::IndexMap::new();
    query.insert(
        "limit".to_string(),
        McpParamSpec {
            kind: "integer".into(),
            description: None,
            required: false,
        },
    );
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query,
        body: None,
    });
    let state = make_state(vec![("accounts", cfg)]).await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": {"name": "accounts", "arguments": {
                "pathParams": {"id": 1042},
                "queryParams": {"limit": 10, "verbose": true}
            }}
        }),
    )
    .await;
    // Not a protocol error — coercion + validation succeeded; the call
    // produced a tool result (its isError may be true since no process
    // backs the synthetic function, which is fine).
    assert!(
        body.get("error").is_none(),
        "scalar args must coerce, got {body}"
    );
    assert!(body["result"].is_object(), "{body}");
}

#[tokio::test]
async fn tools_call_body_object_is_serialized_to_string() {
    // Typed body schemas invite clients to send a JSON object — riz must
    // serialize it into the Lambda event's string body, not reject it.
    let mut cfg = base_cfg(vec![("POST", "/orders")]);
    cfg.mcp = Some(McpToolConfig {
        description: None,
        query: Default::default(),
        body: Some(serde_json::json!({"type":"object"})),
    });
    let state = make_state(vec![("orders", cfg)]).await;
    let addr = serve(state).await;
    let body = rpc(
        addr,
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": {"name": "orders", "arguments": {"body": {"sku":"X1","qty":2}}}
        }),
    )
    .await;
    assert!(body.get("error").is_none(), "{body}");
}

// ───────────────────────────── config validation ────────────────────────────

#[test]
fn bad_mcp_query_type_is_rejected_at_validate() {
    let toml_src = r#"
[function.search]
runtime = "bun"
handler = "./search.ts"
routes = [{ path = "/search", method = "GET" }]

[function.search.mcp.query.limit]
type = "float64"
"#;
    let cfg: Config = toml::from_str(toml_src).expect("parses as toml");
    let err = cfg.validate().expect_err("float64 is not a valid param type");
    assert!(err.contains("search"), "{err}");
    assert!(err.contains("limit"), "{err}");
    assert!(err.contains("float64"), "{err}");
}

#[test]
fn mcp_body_must_be_a_schema_object() {
    let toml_src = r#"
[function.orders]
runtime = "bun"
handler = "./orders.ts"
routes = [{ path = "/orders", method = "POST" }]

[function.orders.mcp]
body = "just a string"
"#;
    let cfg: Config = toml::from_str(toml_src).expect("parses as toml");
    let err = cfg.validate().expect_err("body schema must be an object");
    assert!(err.contains("orders"), "{err}");
}

#[test]
fn valid_mcp_block_parses_and_validates() {
    let toml_src = r#"
[function.accounts]
runtime = "bun"
handler = "./accounts.ts"
routes = [{ path = "/accounts/{id}", method = "GET" }]

[function.accounts.mcp]
description = "Look up an account"

[function.accounts.mcp.query.limit]
type = "integer"
description = "Max results"
required = true
"#;
    let cfg: Config = toml::from_str(toml_src).expect("parses");
    cfg.validate().expect("validates");
    let f = cfg.functions.get("accounts").unwrap();
    let mcp = f.mcp.as_ref().unwrap();
    assert_eq!(mcp.description.as_deref(), Some("Look up an account"));
    assert_eq!(mcp.query.get("limit").unwrap().kind, "integer");
    assert!(mcp.query.get("limit").unwrap().required);
}
