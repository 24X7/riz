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

[function.echo]
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
concurrency = 1

[[function.echo.routes]]
path = "/echo"
method = "GET"
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    for (name, cfg) in &config.functions {
        riz_state.register(riz::state::FunctionState::user(name.clone(), cfg.clone())).await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager.spawn_all(&config.functions, &registry, log_tx.clone()).await.unwrap();

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config.functions.iter()
        .map(|(name, cfg)| Arc::new(
            riz::runtime::process::ProcessHandler::for_function(name, cfg, process_manager.clone())
        ) as Arc<dyn riz::runtime::LambdaHandler>)
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

[function.cached]
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
cache_ttl_secs = 60
concurrency = 1

[[function.cached.routes]]
path = "/cached"
method = "GET"
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    for (name, cfg) in &config.functions {
        riz_state.register(riz::state::FunctionState::user(name.clone(), cfg.clone())).await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager.spawn_all(&config.functions, &registry, log_tx.clone()).await.unwrap();

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config.functions.iter()
        .map(|(name, cfg)| Arc::new(
            riz::runtime::process::ProcessHandler::for_function(name, cfg, process_manager.clone())
        ) as Arc<dyn riz::runtime::LambdaHandler>)
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

    state_for_check.cache.sync().await;

    assert_eq!(state_for_check.cache.entry_count(), 1);
    // After two requests: 1 cache miss + 1 cache hit
    let functions = state_for_check.riz_state.functions.read().await;
    let f = functions.get("cached").unwrap();
    use std::sync::atomic::Ordering;
    assert_eq!(f.cache_hits.load(Ordering::Relaxed), 1);
    assert_eq!(f.cache_misses.load(Ordering::Relaxed), 1);
}
