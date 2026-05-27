//! Layer 1 — HTTP boundary golden tests. These pin the externally observable
//! behavior of the server. Every test must still pass through any refactor.

use std::net::SocketAddr;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::config::{Config, FunctionConfig, RuntimeKind};

fn make_state_with_functions(
    functions: IndexMap<String, FunctionConfig>,
) -> Arc<riz::state::AppState> {
    let config = Config {
        functions,
        ..Default::default()
    };
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    // Build one ProcessHandler per declared function (no spawn — these tests
    // exercise the routing surface and pre-invoke body/cache paths).
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config
        .functions
        .iter()
        .map(|(name, cfg)| {
            let h = riz::runtime::process::ProcessHandler::for_function(
                name,
                cfg,
                process_manager.clone(),
            );
            Arc::new(h) as Arc<dyn riz::runtime::LambdaHandler>
        })
        .collect();
    let router = riz::router::Router::new(handlers);

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
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
async fn health_returns_200_ok_json() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_200_when_all_pools_healthy() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/ready")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/no-such-route"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn deploy_without_auth_returns_503() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/deploy"))
        .json(&serde_json::json!({
            "lambda": "x",
            "s3_bucket": "b",
            "s3_key": "k"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn cache_invalidate_with_keys_returns_evicted_count() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/cache/invalidate"))
        .json(&serde_json::json!({"keys":["nonexistent"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["evicted"].is_number());
}

#[tokio::test]
async fn oversized_body_returns_413_for_routed_request() {
    // The 10 MB body cap is enforced inside dispatch_lambda AFTER route match.
    // A request to a path with no route gets 404 before the body is read, so
    // we register a synthetic function and target its path with an oversized body.
    let mut functions = IndexMap::new();
    functions.insert(
        "sink".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 1000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![riz::config::RouteSpec {
                path: "/sink".into(),
                method: "POST".into(),
            }],
            cors: None,
        },
    );
    let state = make_state_with_functions(functions);
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let big_body = vec![b'x'; 11 * 1024 * 1024];
    let resp = client
        .post(format!("http://{addr}/sink"))
        .body(big_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
}
