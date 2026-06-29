//! Cross-runtime echo parity — Slice A of the runtime-parity matrix.
//!
//! Boots a real riz server for each shipped runtime (Bun, Python, Rust),
//! fires `GET /echo`, and asserts the response body has IDENTICAL field
//! shape across all three. Catches wire-protocol drift between adapters.
//!
//! Canonical shape (all three echo handlers MUST emit these fields):
//!   - echo: string  (mirror of `rawPath`)
//!   - method: string  (e.g. "GET")
//!   - functionName: string  (the riz.toml function name)
//!   - invokedFunctionArn: string  (synthetic `arn:riz:lambda:local:...`)
//!   - awsRequestId: string
//!   - remainingMs: number  (>0 and <= configured timeout_ms)
//!
//! Each test skips gracefully if its runtime's toolchain is absent.
//! Run with: `cargo nextest run --test runtime_parity_echo`

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// ---------- toolchain detection ----------

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok()
}

fn echo_rust_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("release").join("echo-rust")
}

fn echo_rust_available() -> bool {
    let bin = echo_rust_binary();
    bin.exists()
        && std::fs::metadata(&bin)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
}

// ---------- shared server boot ----------

/// Boot a full riz server with the supplied config TOML, return the bound
/// socket address. The server runs forever in a background tokio task;
/// the test process exits when nextest finishes.
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

/// Poll the URL until it responds with any HTTP status (proves the server
/// is up). Times out after 15s.
async fn wait_for_ready(url: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if reqwest::get(url).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server at {url} did not respond within 15s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------- canonical assertion ----------

fn assert_canonical_echo_shape(
    body: &serde_json::Value,
    expected_function_name: &str,
    expected_path: &str,
    expected_method: &str,
    timeout_ms: i64,
) {
    assert_eq!(
        body["echo"], expected_path,
        "echo must mirror rawPath; body = {body}"
    );
    assert_eq!(
        body["method"], expected_method,
        "method must be echoed; body = {body}"
    );
    assert_eq!(
        body["functionName"], expected_function_name,
        "functionName must match riz.toml fn name; body = {body}"
    );
    assert_eq!(
        body["invokedFunctionArn"],
        format!("arn:riz:lambda:local:000000000000:function:{expected_function_name}"),
        "invokedFunctionArn must be the synthetic riz ARN; body = {body}"
    );
    assert!(
        body["awsRequestId"].is_string() && !body["awsRequestId"].as_str().unwrap().is_empty(),
        "awsRequestId must be a non-empty string; body = {body}"
    );
    let remaining = body["remainingMs"]
        .as_i64()
        .unwrap_or_else(|| panic!("remainingMs must be a number; body = {body}"));
    assert!(
        remaining > 0 && remaining <= timeout_ms,
        "remainingMs out of range [1, {timeout_ms}]: got {remaining}; body = {body}"
    );
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_emits_canonical_shape() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let handler = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/lambdas/echo-bun/index.handler"
    );
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-bun]
runtime = "bun"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-bun.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let url = format!("http://{addr}/echo");
    wait_for_ready(&url).await;
    let resp = reqwest::get(&url).await.expect("GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_canonical_echo_shape(&body, "echo-bun", "/echo", "GET", TIMEOUT_MS);
}

#[tokio::test]
async fn python_echo_emits_canonical_shape() {
    if !python3_available() {
        eprintln!("SKIP: python3 not on PATH");
        return;
    }
    let handler = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/lambdas/echo-python/main.lambda_handler"
    );
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-python]
runtime = "python"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-python.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let url = format!("http://{addr}/echo");
    wait_for_ready(&url).await;
    let resp = reqwest::get(&url).await.expect("GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_canonical_echo_shape(&body, "echo-python", "/echo", "GET", TIMEOUT_MS);
}

#[tokio::test]
async fn rust_echo_emits_canonical_shape() {
    if !echo_rust_available() {
        eprintln!(
            "SKIP: echo-rust binary not built at {}. \
             Run `cargo build --release -p echo-rust` first.",
            echo_rust_binary().display()
        );
        return;
    }
    let handler = echo_rust_binary();
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-rust]
runtime = "rust"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-rust.routes]]
path = "/echo"
method = "GET"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    let url = format!("http://{addr}/echo");
    wait_for_ready(&url).await;
    let resp = reqwest::get(&url).await.expect("GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_canonical_echo_shape(&body, "echo-rust", "/echo", "GET", TIMEOUT_MS);
}

#[tokio::test]
async fn node_echo_emits_canonical_shape() {
    if !node_available() {
        eprintln!("SKIP: node not on PATH");
        return;
    }
    let handler = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/lambdas/echo-node/index.handler"
    );
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-node]
runtime = "node"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-node.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let url = format!("http://{addr}/echo");
    wait_for_ready(&url).await;
    let resp = reqwest::get(&url).await.expect("GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_canonical_echo_shape(&body, "echo-node", "/echo", "GET", TIMEOUT_MS);
}
