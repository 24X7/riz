//! Cross-runtime binary body parity — Slice G (inbound base64).
//!
//! When a client POSTs a non-UTF8 body, riz must:
//!   1. Detect the body is not valid UTF-8
//!   2. base64-encode it
//!   3. Set `event.isBase64Encoded = true` in the event sent to the handler
//!
//! This test POSTs non-UTF8 bytes (a PNG header) to each runtime's echo
//! handler, then asserts:
//!   - response.body.isBase64Encoded == true
//!   - base64-decode(response.body.body) == original POSTed bytes
//!
//! Run: `cargo nextest run --test runtime_parity_binary`

use base64::Engine;
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
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
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
        metrics,
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

/// Non-UTF8 byte sequence: PNG file magic header. Guarantees riz takes the
/// is_base64_encoded path (`String::from_utf8(body).is_err()` in server.rs).
const PNG_HEADER: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // \x89PNG\r\n\x1a\n
    0x00, 0x00, 0x00, 0x0D, // IHDR length
    0x49, 0x48, 0x44, 0x52, // "IHDR"
];

async fn exercise_binary_body(addr: SocketAddr, function_name: &str) {
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    wait_for_ready(&client, &url).await;

    let resp = client
        .post(&url)
        .header("content-type", "application/octet-stream")
        .body(PNG_HEADER.to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200, "{function_name}: POST /echo expected 200");
    let body: serde_json::Value = resp.json().await.expect("response is JSON");

    // riz must have base64-encoded the body and set the flag.
    assert_eq!(
        body["isBase64Encoded"], true,
        "{function_name}: riz must mark non-UTF8 body as isBase64Encoded; body = {body}"
    );

    let encoded = body["body"]
        .as_str()
        .unwrap_or_else(|| panic!("{function_name}: body must be a base64 string; body = {body}"));
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|e| panic!("{function_name}: body must be valid base64: {e}; body = {body}"));
    assert_eq!(
        decoded, PNG_HEADER,
        "{function_name}: base64-decode(body) must equal the POSTed bytes; got {decoded:?}"
    );
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_handles_binary_body() {
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
method = "POST"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_binary_body(addr, "echo-bun").await;
}

#[tokio::test]
async fn python_echo_handles_binary_body() {
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
method = "POST"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_binary_body(addr, "echo-python").await;
}

#[tokio::test]
async fn rust_echo_handles_binary_body() {
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
method = "POST"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    exercise_binary_body(addr, "echo-rust").await;
}
