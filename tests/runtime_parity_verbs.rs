//! Cross-runtime HTTP verb + body parity — Slice C of the runtime-parity matrix.
//!
//! For each shipped runtime (Bun, Python, Rust), boot a real riz server with
//! the echo function declared as method = "ANY", fire GET / POST / PUT / PATCH /
//! DELETE through the public HTTP port, and assert:
//!   - status 200 every time
//!   - response body's `method` field echoes the request method
//!   - response body's `echo` field echoes the request path
//!   - response body's `body` field echoes the request body (or null for GET/DELETE)
//!
//! Run: `cargo nextest run --test runtime_parity_verbs`

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

// ---------- canonical verb exerciser ----------

/// For each HTTP verb, fire a request and assert the echo handler returned
/// the right method + body. `body_text` of empty string means no body was
/// sent (GET / DELETE patterns).
async fn exercise_all_verbs(addr: SocketAddr, function_name: &str) {
    let url = format!("http://{addr}/echo");
    let client = reqwest::Client::new();
    wait_for_ready(&client, &url).await;

    struct Case {
        method: &'static str,
        body: Option<&'static str>,
    }
    let cases = [
        Case {
            method: "GET",
            body: None,
        },
        Case {
            method: "POST",
            body: Some(r#"{"name":"alice"}"#),
        },
        Case {
            method: "PUT",
            body: Some(r#"{"name":"bob"}"#),
        },
        Case {
            method: "PATCH",
            body: Some(r#"[{"op":"replace","path":"/n","value":1}]"#),
        },
        Case {
            method: "DELETE",
            body: None,
        },
    ];

    for case in &cases {
        let req = match case.method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => unreachable!(),
        };
        let req = match case.body {
            Some(b) => req.header("content-type", "application/json").body(b),
            None => req,
        };
        let resp = req.send().await.expect("send");
        assert_eq!(
            resp.status(),
            200,
            "{function_name} {method} expected 200",
            method = case.method
        );
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(
            body["method"],
            case.method,
            "{function_name} {method}: response.method mismatch; full body = {body}",
            method = case.method
        );
        assert_eq!(
            body["echo"],
            "/echo",
            "{function_name} {method}: response.echo mismatch; full body = {body}",
            method = case.method
        );
        match case.body {
            Some(sent) => assert_eq!(
                body["body"], sent,
                "{function_name} {method}: request body must round-trip; full body = {body}",
                method = case.method
            ),
            None => assert!(
                body["body"].is_null() || body["body"] == "",
                "{function_name} {method}: bodyless request must produce null/empty body; full body = {body}",
                method = case.method
            ),
        }
    }
}

// ---------- per-runtime tests ----------

const TIMEOUT_MS: i64 = 5000;

#[tokio::test]
async fn bun_echo_handles_all_verbs() {
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
method = "ANY"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_all_verbs(addr, "echo-bun").await;
}

#[tokio::test]
async fn node_echo_handles_all_verbs() {
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
method = "ANY"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_all_verbs(addr, "echo-node").await;
}

#[tokio::test]
async fn python_echo_handles_all_verbs() {
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
method = "ANY"
"#
    );
    let addr = boot_riz(&config_toml).await;
    exercise_all_verbs(addr, "echo-python").await;
}

#[tokio::test]
async fn rust_echo_handles_all_verbs() {
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
method = "ANY"
"#,
        handler = handler.display()
    );
    let addr = boot_riz(&config_toml).await;
    exercise_all_verbs(addr, "echo-rust").await;
}
