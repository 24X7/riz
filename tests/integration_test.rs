//! Integration test: starts riz with a real Bun echo lambda and fires HTTP requests.
//! Requires `bun` to be installed on PATH.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires bun on PATH"]
async fn echo_lambda_returns_200() {
    let config_toml = format!(r#"
[server]
port = 0
host = "127.0.0.1"

[[routes]]
path = "/echo"
method = "GET"
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
concurrency = 1
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    for route in &config.routes {
        let key = riz::router::Router::route_key(&route.method, &route.path);
        riz_state.register(riz::state::FunctionState::user(key, route.clone())).await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager.spawn_all(&config.routes, &registry, log_tx.clone()).await.unwrap();

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config.routes.iter()
        .map(|r| Arc::new(riz::runtime::process::ProcessHandler::for_route(r, process_manager.clone()))
            as Arc<dyn riz::runtime::LambdaHandler>)
        .collect();
    let router = riz::router::Router::new(handlers);

    let app_state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
    });

    // Bind to random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app = riz::server::build_app(app_state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/echo");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&url).await.is_ok() { break; }
        assert!(tokio::time::Instant::now() < deadline, "server did not start within 10s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["echo"], "/echo");
    assert_eq!(body["method"], "GET");
}

#[tokio::test]
#[ignore = "requires bun on PATH"]
async fn cache_returns_hit_on_second_request() {
    let config_toml = format!(r#"
[server]
port = 0
host = "127.0.0.1"

[[routes]]
path = "/cached"
method = "GET"
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
cache_ttl_secs = 60
concurrency = 1
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    for route in &config.routes {
        let key = riz::router::Router::route_key(&route.method, &route.path);
        riz_state.register(riz::state::FunctionState::user(key, route.clone())).await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager.spawn_all(&config.routes, &registry, log_tx.clone()).await.unwrap();

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config.routes.iter()
        .map(|r| Arc::new(riz::runtime::process::ProcessHandler::for_route(r, process_manager.clone()))
            as Arc<dyn riz::runtime::LambdaHandler>)
        .collect();
    let router = riz::router::Router::new(handlers);

    let state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();
    let state_for_check = state.clone();

    tokio::spawn(async move {
        let app = riz::server::build_app(state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/cached");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&url).await.is_ok() { break; }
        assert!(tokio::time::Instant::now() < deadline, "server did not start within 10s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let r1 = reqwest::get(&url).await.unwrap();
    assert_eq!(r1.status(), 200);
    let r2 = reqwest::get(&url).await.unwrap();
    assert_eq!(r2.status(), 200);

    // Flush moka's pending write ops so entry_count is accurate
    state_for_check.cache.sync().await;

    // After two requests: 1 cache miss + 1 hit → 1 cached entry
    assert_eq!(state_for_check.cache.entry_count(), 1);
    let stats = state_for_check.route_stats.read().await;
    let route_stats = stats.get("GET /cached").unwrap();
    use std::sync::atomic::Ordering;
    assert_eq!(route_stats.cache_hits.load(Ordering::Relaxed), 1);
    assert_eq!(route_stats.cache_misses.load(Ordering::Relaxed), 1);
}
