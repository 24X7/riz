//! Layer 3 — full HTTP integration for the /_riz/* system endpoints.
//! Spins up the assembled server with a synthetic user function (no real
//! lambda process) and verifies each endpoint reflects expected state.

use std::net::SocketAddr;
use std::sync::Arc;

async fn make_state() -> Arc<riz::state::AppState> {
    let config = riz::config::Config {
        server: Default::default(),
        cache: Default::default(),
        datadog: Default::default(),
        deploy: Default::default(),
        aws: Default::default(),
        routes: vec![],
    };
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    riz_state.register(riz::state::FunctionState::system("GET /_riz/health")).await;
    riz_state.register(riz::state::FunctionState::system("GET /_riz/metrics")).await;
    riz_state.register(riz::state::FunctionState::system("GET /_riz/registry")).await;
    riz_state.register(riz::state::FunctionState::system("POST /_riz/mcp")).await;

    // Synthetic user function (no real process spawned).
    let route = riz::config::RouteConfig {
        path: "/echo".into(),
        method: "GET".into(),
        runtime: riz::config::RuntimeKind::Bun,
        handler: std::path::PathBuf::from("./echo.ts"),
        timeout_ms: 5000,
        cache_ttl_secs: None,
        concurrency: 1,
    };
    riz_state.register(riz::state::FunctionState::user("GET /echo", route)).await;
    // Pre-record an invocation so health/metrics have a non-zero value to assert against.
    riz_state.record_invocation("GET /echo", 12.5, true, false).await;

    let mcp = Arc::new(riz::system::mcp::McpHandler::new(riz_state.clone()));
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = vec![
        Arc::new(riz::system::health::HealthHandler::new(riz_state.clone())),
        Arc::new(riz::system::metrics::MetricsHandler::new(riz_state.clone())),
        Arc::new(riz::system::registry::RegistryHandler::new(riz_state.clone())),
        mcp.clone() as Arc<dyn riz::runtime::LambdaHandler>,
    ];
    let router_arc = Arc::new(riz::router::Router::new(handlers.clone()));
    mcp.set_router(router_arc).await;

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(handlers)),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
    })
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn health_endpoint_reports_user_function() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    let functions = body["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["route_key"] == "GET /echo"));
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/metrics")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("text/plain"));
    let body = resp.text().await.unwrap();
    assert!(body.contains("riz_invocations_total{route=\"GET /echo\"} 1"), "{body}");
    assert!(body.contains("riz_uptime_seconds"));
}

#[tokio::test]
async fn registry_endpoint_lists_user_and_system_functions() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/registry")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let functions = body["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["kind"] == "system" && f["path"] == "/_riz/health"));
    assert!(functions.iter().any(|f| f["kind"] == "user" && f["path"] == "/echo"));
}

#[tokio::test]
async fn mcp_tools_list_includes_user_function() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let resp = client.post(format!("http://{addr}/_riz/mcp"))
        .json(&req).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "GET_echo"));
}

#[tokio::test]
async fn mcp_unknown_method_returns_jsonrpc_error() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"nope"});
    let resp = client.post(format!("http://{addr}/_riz/mcp"))
        .json(&req).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601);
}
