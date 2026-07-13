//! Integration test: starts riz with a real Bun echo lambda and fires HTTP requests.
//! Requires `bun` to be installed on PATH.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Sanity check that the Bun adapter asset is embedded in the binary.
/// This test runs without Bun — it only checks that the source asset exists.
#[test]
fn bun_adapter_asset_exists() {
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bun-adapter.mjs");
    assert!(
        adapter.exists(),
        "assets/bun-adapter.mjs must exist — Bun integration requires this adapter"
    );
}

/// Sanity check that the echo lambda fixture exists and is non-empty.
/// This test runs without Bun — it validates the test fixture setup.
#[test]
fn echo_lambda_fixture_exists() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/echo-lambda/index.ts");
    assert!(
        fixture.exists(),
        "tests/fixtures/echo-lambda/index.ts must exist — Bun integration test fixture"
    );
    let content = std::fs::read_to_string(&fixture).expect("fixture must be readable");
    assert!(
        content.contains("export const handler"),
        "echo lambda fixture must export a handler function"
    );
}

#[tokio::test]
async fn echo_lambda_returns_200() {
    let config_toml = format!(
        r#"
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
"#,
        handler = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/echo-lambda/index.ts"
        )
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
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
        .unwrap();

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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/echo");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&url).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server did not start within 10s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["echo"], "/echo");
    assert_eq!(body["method"], "GET");
    assert_eq!(body["functionName"], "echo");
    assert_eq!(
        body["invokedFunctionArn"],
        "arn:riz:lambda:local:000000000000:function:echo"
    );
    assert!(
        body["awsRequestId"].is_string(),
        "awsRequestId must be a string"
    );
    let remaining = body["remainingMs"]
        .as_i64()
        .expect("remainingMs must be a number");
    // Echo lambda has timeout_ms = 5000; remaining must be positive and <= 5000.
    assert!(
        remaining > 0 && remaining <= 5000,
        "remainingMs out of range: {remaining}"
    );
}

#[tokio::test]
async fn cache_returns_hit_on_second_request() {
    let config_toml = format!(
        r#"
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
"#,
        handler = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/echo-lambda/index.ts"
        )
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
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
        .unwrap();

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

    let state = Arc::new(riz::state::AppState {
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();
    let state_for_check = state.clone();

    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/cached");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&url).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server did not start within 10s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let r1 = reqwest::get(&url).await.unwrap();
    assert_eq!(r1.status(), 200);
    let r2 = reqwest::get(&url).await.unwrap();
    assert_eq!(r2.status(), 200);

    state_for_check.cache.sync().await;

    assert_eq!(state_for_check.cache.entry_count(), 1);
    // Three requests total: warm-up (cache miss), r1 (cache hit), r2 (cache hit).
    // The warm-up primes the cache; r1 and r2 both return the cached entry.
    let functions = state_for_check.riz_state.functions.read().await;
    let f = functions.get("cached").unwrap();
    use std::sync::atomic::Ordering;
    assert_eq!(f.cache_hits.load(Ordering::Relaxed), 2);
    assert_eq!(f.cache_misses.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn echo_lambda_round_trips_put_delete_patch() {
    // Register the echo lambda on `method = "ANY"` so all five verbs hit the
    // same process pool. Then dispatch PUT/DELETE/PATCH and assert the
    // echoed method on each response matches the AWS contract path —
    // i.e., `event.requestContext.http.method` carries through unchanged.
    let config_toml = format!(
        r#"
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
method = "ANY"
"#,
        handler = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/echo-lambda/index.ts"
        )
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
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
        .unwrap();

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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/echo");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let client = reqwest::Client::new();
    loop {
        if client.get(&url).send().await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server did not start within 10s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    for method in ["PUT", "DELETE", "PATCH"] {
        let req = match method {
            "PUT" => client.put(&url).body("{\"name\":\"Alice\"}"),
            "DELETE" => client.delete(&url),
            "PATCH" => client
                .patch(&url)
                .body("[{\"op\":\"replace\",\"path\":\"/n\",\"value\":1}]"),
            _ => unreachable!(),
        };
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 200, "{method} expected 200");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["echo"], "/echo", "{method} echoed path");
        assert_eq!(body["method"], method, "{method} echoed method");
    }
}

/// Real workload: fire 1 000 concurrent GET requests through the echo function
/// and assert that every one returns 200 (zero 5xx).
///
/// Gate: `cargo nextest run --include-ignored` (or `--run-ignored all`).
/// Requires `bun` on PATH. The test skips gracefully when bun is absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-workload test: run with --include-ignored; requires bun"]
async fn echo_lambda_1000_requests_no_5xx() {
    // Skip if bun is not present.
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("echo_lambda_1000_requests_no_5xx: bun not on PATH — skipping");
        return;
    }

    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo]
runtime = "bun"
handler = "{handler}"
timeout_ms = 10000
concurrency = 4

[[function.echo.routes]]
path = "/echo"
method = "GET"
"#,
        handler = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/echo-lambda/index.ts"
        )
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(100_000);

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
        .unwrap();

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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{bound_addr}/echo");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    // Wait for first successful response.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if client.get(&url).send().await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server did not start within 15s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Fire 1 000 concurrent requests.
    const N: usize = 1_000;
    let futs: Vec<_> = (0..N)
        .map(|_| {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move { client.get(&url).send().await.map(|r| r.status().as_u16()) })
        })
        .collect();

    let mut five_xx_count = 0usize;
    for fut in futs {
        match fut.await {
            Ok(Ok(status)) if status >= 500 => five_xx_count += 1,
            Ok(Err(_)) => five_xx_count += 1, // connection error counts as failure
            _ => {}
        }
    }

    assert_eq!(
        five_xx_count, 0,
        "real workload: {five_xx_count}/{N} requests returned 5xx — zero expected"
    );
}
