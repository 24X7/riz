//! Integration test: starts riz with a real Python echo lambda and fires HTTP requests.
//! Gated on `python3` being available on PATH.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Returns the path to `python3` if it is available on PATH, or `None`.
fn python3_path() -> Option<String> {
    let out = std::process::Command::new("which")
        .arg("python3")
        .output()
        .ok()?;
    if out.status.success() {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !path.is_empty() {
            return Some(path);
        }
    }
    None
}

#[tokio::test]
async fn python_echo_lambda_returns_200() {
    if python3_path().is_none() {
        eprintln!("SKIP: python3 not on PATH — skipping python_echo_lambda_returns_200");
        return;
    }

    let handler_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/echo-python/main.lambda_handler"
    );

    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-python]
runtime = "python"
handler = "{handler}"
timeout_ms = 5000
concurrency = 1

[[function.echo-python.routes]]
path = "/echo-python"
method = "GET"
"#,
        handler = handler_path
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

    let url = format!("http://{bound_addr}/echo-python");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if reqwest::get(&url).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "python server did not start within 15s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["echo"], "/echo-python",
        "echo field must match rawPath"
    );
    assert_eq!(body["method"], "GET", "method must be echoed");
    assert_eq!(
        body["functionName"], "echo-python",
        "functionName must match the riz.toml function name"
    );
    assert_eq!(
        body["invokedFunctionArn"], "arn:riz:lambda:local:000000000000:function:echo-python",
        "invokedFunctionArn must be the synthetic riz ARN"
    );
    assert!(
        body["awsRequestId"].is_string(),
        "awsRequestId must be a string"
    );
    let remaining = body["remainingMs"]
        .as_i64()
        .expect("remainingMs must be a number");
    assert!(
        remaining > 0 && remaining <= 5000,
        "remainingMs out of range: {remaining}"
    );
}
