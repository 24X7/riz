//! Cross-runtime response headers + Set-Cookie parity — Slice F.
//!
//! For each shipped runtime (Bun, Python, Rust), boots a real riz server,
//! fires GET /echo, and asserts the HTTP client received BOTH:
//!   - a custom response header (`x-riz-echo: ok`) emitted by the handler
//!   - a `Set-Cookie: sid=abc; Path=/` header materialized from the
//!     handler's response.cookies array (AWS API GW v2 cookie shape)
//!
//! Run: `cargo nextest run --test runtime_parity_response`

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

// ---------- canonical assertion ----------

async fn exercise_response_shape(addr: SocketAddr, function_name: &str) {
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    wait_for_ready(&client, &url).await;

    let resp = client.get(&url).send().await.expect("send");
    assert_eq!(resp.status(), 200, "{function_name}: expected 200");

    // Custom response header round-tripped from handler to HTTP client.
    let x_echo = resp
        .headers()
        .get("x-riz-echo")
        .unwrap_or_else(|| panic!("{function_name}: response missing x-riz-echo header"))
        .to_str()
        .expect("ascii");
    assert_eq!(
        x_echo, "ok",
        "{function_name}: x-riz-echo must be \"ok\", got {x_echo:?}"
    );

    // Set-Cookie materialized from handler's response.cookies array.
    // reqwest exposes multiple Set-Cookie headers via get_all().
    let set_cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or("").to_string())
        .collect();
    assert!(
        set_cookies.iter().any(|c| c.contains("sid=abc")),
        "{function_name}: response must include Set-Cookie with sid=abc; got headers = {set_cookies:?}"
    );
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_response_headers_and_cookies() {
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
    exercise_response_shape(addr, "echo-bun").await;
}

#[tokio::test]
async fn node_echo_response_headers_and_cookies() {
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
    exercise_response_shape(addr, "echo-node").await;
}

#[tokio::test]
async fn python_echo_response_headers_and_cookies() {
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
    exercise_response_shape(addr, "echo-python").await;
}

#[tokio::test]
async fn rust_echo_response_headers_and_cookies() {
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
    exercise_response_shape(addr, "echo-rust").await;
}
