//! Cross-runtime path-params + query-string parity — Slice D.
//!
//! For each shipped runtime (Bun, Python, Rust), boots a real riz server
//! with the echo function mounted at `/users/{id}` and fires a GET to
//! `/users/42?name=alice&count=3`. Asserts the handler saw:
//!   - pathParameters.id == "42"        (router-extracted path param)
//!   - queryStringParameters.name == "alice" + count == "3"  (gateway-parsed query)
//!
//! Catches drift in either the router's path-param extraction OR each
//! runtime adapter's event-envelope passthrough.
//!
//! Run: `cargo nextest run --test runtime_parity_request_shape`

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

/// The compiled wasm32-wasip1 echo module (built by
/// `cargo build --release --target wasm32-wasip1` in examples/lambdas/echo-wasm).
fn echo_wasm_module() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/lambdas/echo-wasm/target/wasm32-wasip1/release/echo-wasm.wasm")
}

/// The built riz host binary — needed because WasmRuntime re-invokes
/// `riz __wasm-host`. In-process tests set `RIZ_HOST_BIN` to point at it.
fn riz_host_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("release").join("riz")
}

fn echo_wasm_available() -> bool {
    let m = echo_wasm_module();
    let host = riz_host_binary();
    m.exists()
        && std::fs::metadata(&m).map(|md| md.len() > 0).unwrap_or(false)
        && host.exists()
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

async fn exercise_path_and_query(addr: SocketAddr, function_name: &str) {
    let client = reqwest::Client::new();
    // Health probe to any path on this server — readiness only.
    wait_for_ready(&client, &format!("http://{addr}/users/0")).await;

    let url = format!("http://{addr}/users/42?name=alice&count=3");
    let resp = client.get(&url).send().await.expect("send");
    assert_eq!(resp.status(), 200, "{function_name}: GET /users/42 expected 200");
    let body: serde_json::Value = resp.json().await.expect("json");

    // Path params: router extracted `{id}` → "42".
    let id = &body["pathParameters"]["id"];
    assert_eq!(
        id, "42",
        "{function_name}: pathParameters.id must be \"42\"; full body = {body}"
    );

    // Query string: gateway parsed `name=alice&count=3`.
    let name = &body["queryStringParameters"]["name"];
    let count = &body["queryStringParameters"]["count"];
    assert_eq!(
        name, "alice",
        "{function_name}: queryStringParameters.name must be \"alice\"; full body = {body}"
    );
    assert_eq!(
        count, "3",
        "{function_name}: queryStringParameters.count must be \"3\"; full body = {body}"
    );
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_passes_path_and_query() {
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
path = "/users/{{id}}"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_path_and_query(addr, "echo-bun").await;
}

#[tokio::test]
async fn node_echo_passes_path_and_query() {
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
path = "/users/{{id}}"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_path_and_query(addr, "echo-node").await;
}

#[tokio::test]
async fn python_echo_passes_path_and_query() {
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
path = "/users/{{id}}"
method = "GET"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_path_and_query(addr, "echo-python").await;
}

#[tokio::test]
async fn rust_echo_passes_path_and_query() {
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
path = "/users/{{id}}"
method = "GET"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    exercise_path_and_query(addr, "echo-rust").await;
}

#[tokio::test]
async fn wasm_echo_passes_path_and_query() {
    if !echo_wasm_available() {
        eprintln!(
            "SKIP: echo-wasm module or riz host binary not built. Run \
             `cargo build --release` and \
             `cargo build --release --target wasm32-wasip1` in examples/lambdas/echo-wasm first."
        );
        return;
    }
    // WasmRuntime re-invokes `riz __wasm-host`; in-process tests boot build_app
    // under the nextest binary, so point it at the real riz host binary.
    std::env::set_var("RIZ_HOST_BIN", riz_host_binary());
    let handler = echo_wasm_module();
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-wasm]
runtime = "wasm"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-wasm.routes]]
path = "/users/{{id}}"
method = "GET"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    exercise_path_and_query(addr, "echo-wasm").await;
}
