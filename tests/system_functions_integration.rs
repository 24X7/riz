//! Layer 3 — full HTTP integration for the /_riz/* system endpoints.
//! Spins up the assembled server with a synthetic user function (no real
//! lambda process) and verifies each endpoint reflects expected state.

use std::net::SocketAddr;
use std::sync::Arc;

use riz::config::{Config, FunctionConfig, RuntimeKind};

async fn make_state() -> Arc<riz::state::AppState> {
    let config = Config::default();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_health",
            vec!["GET /_riz/health".into()],
            "$default",
        ))
        .await;
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_metrics",
            vec!["GET /_riz/metrics".into()],
            "$default",
        ))
        .await;
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_registry",
            vec!["GET /_riz/registry".into()],
            "$default",
        ))
        .await;
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_mcp",
            vec!["POST /_riz/mcp".into()],
            "$default",
        ))
        .await;

    // Synthetic user function — registered in state with one invocation
    // pre-recorded so health/metrics assertions have data to look at.
    let cfg = FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from("./echo.ts"),
        timeout_ms: 5000,
        integration_timeout_ms: 30000,
        stage_variables: Default::default(),
        env: Default::default(),
        cache_ttl_secs: None,
        concurrency: 1,
        routes: vec![riz::config::RouteSpec {
            path: "/echo".into(),
            method: "GET".into(),
        }],
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
    riz_state
        .register(riz::state::FunctionState::user("echo", cfg, "$default", 0))
        .await;
    riz_state.record_invocation("echo", 12.5, true, false).await;

    let mcp = Arc::new(riz::system::mcp::McpHandler::new(riz_state.clone(), None));
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = vec![
        Arc::new(riz::system::health::HealthHandler::new(riz_state.clone())),
        Arc::new(riz::system::metrics::MetricsHandler::new(
            riz_state.clone(),
            Arc::new(riz::process::ProcessManager::new(riz_state.clone())),
            None,
            true,
        )),
        Arc::new(riz::system::registry::RegistryHandler::new(
            riz_state.clone(),
            None,
        )),
        mcp.clone() as Arc<dyn riz::runtime::LambdaHandler>,
    ];
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

#[tokio::test]
async fn system_endpoints_respond_with_aws_shape() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    let functions = body["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["name"] == "echo"));
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/metrics"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/plain"));
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("riz_invocations_total{function=\"echo\"} 1"),
        "{body}"
    );
    assert!(body.contains("riz_uptime_seconds"));
}

#[tokio::test]
async fn registry_endpoint_lists_user_and_system_functions() {
    let state = make_state().await;
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/_riz/registry"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let functions = body["functions"].as_array().unwrap();
    assert!(functions
        .iter()
        .any(|f| f["kind"] == "system" && f["name"] == "_riz_health"));
    assert!(functions
        .iter()
        .any(|f| f["kind"] == "user" && f["name"] == "echo"));
}

#[tokio::test]
async fn mcp_tools_list_includes_user_function() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let resp = client
        .post(format!("http://{addr}/_riz/mcp"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    // Tool name is the function name verbatim.
    assert!(tools.iter().any(|t| t["name"] == "echo"));
}

#[tokio::test]
async fn mcp_unknown_method_returns_jsonrpc_error() {
    let state = make_state().await;
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"nope"});
    let resp = client
        .post(format!("http://{addr}/_riz/mcp"))
        .json(&req)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601);
}
