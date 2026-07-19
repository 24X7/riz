//! Cache layer middleware — Slice K.
//!
//! Two tests, both Bun-only (cache runs as middleware in `server.rs`
//! before dispatch — runtime-agnostic).
//!
//! Verification trick: echo-bun increments a per-process `invocationCount`
//! global on every real invocation and emits it in the response body.
//! When the cache returns a stored response, the handler is NOT re-invoked
//! — so a cache hit replays the captured count, while a cache miss
//! shows an incremented count.
//!
//! 1. cache_hit: cache_ttl_secs = 60 → 2nd request returns count = 1
//!    (proves cache returned a stored response without invoking handler)
//!
//! 2. cache_miss: cache_ttl_secs = 0 (caching disabled) → 2nd request
//!    returns count = 2 (proves every request invokes the handler)
//!
//! Run: `cargo nextest run --test middleware_cache`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

async fn boot_riz(config_toml: &str) -> SocketAddr {
    let config: riz::config::Config = toml::from_str(config_toml).expect("toml parses");
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let stage = config.server.stage.clone();
    let default_ttl = config.cache.default_ttl_secs;
    for (name, cfg) in &config.functions {
        riz_state
            .register(riz::state::FunctionState::user(
                name.clone(),
                cfg.clone(),
                &stage,
                default_ttl,
            ))
            .await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager
        .spawn_all(&config.functions, &registry, log_tx.clone())
        .await
        .expect("spawn_all");

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config
        .functions
        .iter()
        .map(|(name, cfg)| {
            Arc::new(riz::runtime::process::ProcessHandler::for_function(
                name,
                cfg,
                process_manager.clone(),
            )) as Arc<dyn riz::runtime::LambdaHandler>
        })
        .collect();
    let router = riz::router::Router::new(handlers);

    let app_state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let bound = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.expect("axum::serve");
    });

    bound
}

async fn wait_for_ready(client: &reqwest::Client, url: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if client.get(url).send().await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server at {url} did not respond within 15s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const TIMEOUT_MS: i64 = 5000;
const ECHO_BUN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/parity/echo-bun/index.handler"
);

async fn invocation_count_at(client: &reqwest::Client, url: &str) -> i64 {
    let resp = client.get(url).send().await.expect("send");
    assert_eq!(resp.status(), 200, "GET {url} expected 200");
    let body: serde_json::Value = resp.json().await.expect("json");
    body["invocationCount"]
        .as_i64()
        .unwrap_or_else(|| panic!("response body missing invocationCount: {body}"))
}

#[tokio::test]
async fn cache_hit_replays_response_without_reinvoking_handler() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-bun]
runtime = "bun"
handler = "{ECHO_BUN}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1
cache_ttl_secs = 60

[[function.echo-bun.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    wait_for_ready(&client, &url).await;

    // First request: cold miss → handler invoked → cache stores response.
    let first = invocation_count_at(&client, &url).await;

    // Second request: cache hit → handler NOT invoked → response replays
    // the same invocationCount captured in the stored response.
    let second = invocation_count_at(&client, &url).await;

    assert_eq!(
        first, second,
        "cache hit must replay first response (got count {first} then {second}); \
         handler ran twice — cache MISS where there should have been a HIT"
    );
}

#[tokio::test]
async fn cache_disabled_invokes_handler_every_time() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    // cache_ttl_secs = 0 → caching disabled per the gating in server.rs:554.
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-bun]
runtime = "bun"
handler = "{ECHO_BUN}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1
cache_ttl_secs = 0

[[function.echo-bun.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    wait_for_ready(&client, &url).await;

    let first = invocation_count_at(&client, &url).await;
    let second = invocation_count_at(&client, &url).await;

    assert_eq!(
        second,
        first + 1,
        "with caching disabled, handler must run each request \
         (got count {first} then {second}; expected {first} then {})",
        first + 1
    );
}
