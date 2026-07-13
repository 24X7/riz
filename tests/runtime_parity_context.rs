//! Cross-runtime stage-variable + cookie + custom-header parity — Slice E.
//!
//! For each shipped runtime (Bun, Python, Rust), boots a real riz server
//! with the echo function declared with stage_variables, fires a GET
//! carrying both a custom header (`x-test-key: hello`) and a Cookie
//! header (`session=abc; theme=dark`), and asserts the handler observed:
//!   - stageVariables.tier == "production"  (from `[function.X.stage_variables]`)
//!   - requestHeaders["x-test-key"] == "hello"
//!   - cookies contains "session=abc" and "theme=dark"
//!
//! Run: `cargo nextest run --test runtime_parity_context`

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
        // Wait for SUCCESS, not just any response: a connectable server whose
        // runtime pool is still cold answers 5xx, which would race the test.
        if let Ok(r) = client.get(url).send().await {
            if r.status().is_success() {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server at {url} did not respond within 15s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------- canonical assertion ----------

async fn exercise_context_fields(addr: SocketAddr, function_name: &str) {
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    wait_for_ready(&client, &url).await;

    let resp = client
        .get(&url)
        .header("x-test-key", "hello")
        .header("cookie", "session=abc; theme=dark")
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        200,
        "{function_name}: GET /echo expected 200"
    );
    let body: serde_json::Value = resp.json().await.expect("json");

    // Stage variables from [function.X.stage_variables] block.
    assert_eq!(
        body["stageVariables"]["tier"], "production",
        "{function_name}: stageVariables.tier must be \"production\"; body = {body}"
    );

    // Custom request header — AWS API GW v2 lowercases header keys.
    assert_eq!(
        body["requestHeaders"]["x-test-key"], "hello",
        "{function_name}: requestHeaders[x-test-key] must be \"hello\"; body = {body}"
    );

    // Cookies — AWS v2 puts cookies in event.cookies (array of "k=v" strings),
    // not in event.headers.cookie.
    let cookies = body["cookies"]
        .as_array()
        .unwrap_or_else(|| panic!("{function_name}: cookies must be an array; body = {body}"));
    let joined: String = cookies
        .iter()
        .filter_map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        joined.contains("session=abc"),
        "{function_name}: cookies must contain session=abc; got {joined:?}; body = {body}"
    );
    assert!(
        joined.contains("theme=dark"),
        "{function_name}: cookies must contain theme=dark; got {joined:?}; body = {body}"
    );
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_passes_context_fields() {
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

[function.echo-bun.stage_variables]
tier = "production"

[[function.echo-bun.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_context_fields(addr, "echo-bun").await;
}

#[tokio::test]
async fn node_echo_passes_context_fields() {
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

[function.echo-node.stage_variables]
tier = "production"

[[function.echo-node.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_context_fields(addr, "echo-node").await;
}

#[tokio::test]
async fn python_echo_passes_context_fields() {
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

[function.echo-python.stage_variables]
tier = "production"

[[function.echo-python.routes]]
path = "/echo"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_context_fields(addr, "echo-python").await;
}

#[tokio::test]
async fn rust_echo_passes_context_fields() {
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

[function.echo-rust.stage_variables]
tier = "production"

[[function.echo-rust.routes]]
path = "/echo"
method = "GET"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    exercise_context_fields(addr, "echo-rust").await;
}
