//! Integration test: starts osbox with a real Bun echo lambda and fires HTTP requests.
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

    let config: osbox::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = osbox::process::runtime::RuntimeRegistry::new().unwrap();
    let cache = osbox::cache::CacheLayer::new(&config.cache);
    let metrics = osbox::metrics::MetricsEmitter::new(&config.datadog);
    let router = osbox::router::Router::new(config.routes.clone());
    let process_manager = osbox::process::ProcessManager::new();
    process_manager.spawn_all(&config.routes, &registry).await.unwrap();

    let app_state = Arc::new(osbox::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    // Bind to random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app = osbox::server::build_app(app_state)
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

    let config: osbox::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = osbox::process::runtime::RuntimeRegistry::new().unwrap();
    let cache = osbox::cache::CacheLayer::new(&config.cache);
    let metrics = osbox::metrics::MetricsEmitter::new(&config.datadog);
    let router = osbox::router::Router::new(config.routes.clone());
    let process_manager = osbox::process::ProcessManager::new();
    process_manager.spawn_all(&config.routes, &registry).await.unwrap();

    let state = Arc::new(osbox::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();
    let state_for_check = state.clone();

    tokio::spawn(async move {
        let app = osbox::server::build_app(state)
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
    assert_eq!(route_stats.cache_hits, 1);
    assert_eq!(route_stats.cache_misses, 1);
}
