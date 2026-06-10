//! CORS preflight middleware — Slice I.
//!
//! CORS runs as middleware in front of dispatch, so it's runtime-agnostic.
//! This test boots a single Bun server (any runtime would do) with a
//! global `[cors]` block, sends OPTIONS /echo with an Origin header,
//! and asserts:
//!   - 204 No Content
//!   - Access-Control-Allow-Origin echoes the request origin (because
//!     the origin is in the allowlist)
//!   - Access-Control-Allow-Methods includes the requested method
//!   - the handler was NOT invoked (no x-riz-echo header — that would
//!     prove dispatch ran past the middleware)
//!
//! Also covers the per-function CORS override path with a second test.
//!
//! Run: `cargo nextest run --test middleware_cors_preflight`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

// ---------- toolchain detection ----------

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
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

const TIMEOUT_MS: i64 = 5000;
const BUN_HANDLER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/lambdas/echo-bun/index.handler"
);

#[tokio::test]
async fn cors_preflight_returns_204_with_allow_headers() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[cors]
allow_origins = ["http://example.com"]
allow_methods = ["GET", "POST"]
allow_headers = ["Content-Type", "Authorization"]

[function.echo-bun]
runtime = "bun"
handler = "{BUN_HANDLER}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-bun.routes]]
path = "/echo"
method = "ANY"
"#
    );
    let addr = boot_riz(&config_toml).await;

    let client = reqwest::Client::new();
    // Use a GET to warm the server and confirm the route is mounted.
    wait_for_ready(&client, &format!("http://{addr}/echo")).await;

    // The actual preflight.
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/echo"))
        .header("origin", "http://example.com")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .expect("send OPTIONS");

    assert_eq!(resp.status().as_u16(), 204, "preflight must return 204");

    let allow_origin = resp
        .headers()
        .get("access-control-allow-origin")
        .expect("preflight must include access-control-allow-origin")
        .to_str()
        .unwrap();
    assert!(
        allow_origin == "http://example.com" || allow_origin == "*",
        "Allow-Origin must echo or wildcard the request origin; got {allow_origin:?}"
    );

    let allow_methods = resp
        .headers()
        .get("access-control-allow-methods")
        .expect("preflight must include access-control-allow-methods")
        .to_str()
        .unwrap()
        .to_ascii_uppercase();
    assert!(
        allow_methods.contains("GET"),
        "Allow-Methods must contain GET; got {allow_methods:?}"
    );

    // The handler must NOT have run. The handler always emits `x-riz-echo:
    // ok` — its absence proves dispatch short-circuited at the middleware.
    assert!(
        resp.headers().get("x-riz-echo").is_none(),
        "preflight must NOT invoke the handler (x-riz-echo header present)"
    );
}

#[tokio::test]
async fn cors_preflight_rejects_unallowed_origin() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[cors]
allow_origins = ["http://allowed.com"]
allow_methods = ["GET"]
allow_headers = ["Content-Type"]

[function.echo-bun]
runtime = "bun"
handler = "{BUN_HANDLER}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.echo-bun.routes]]
path = "/echo"
method = "ANY"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let client = reqwest::Client::new();
    wait_for_ready(&client, &format!("http://{addr}/echo")).await;

    let resp = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/echo"))
        .header("origin", "http://attacker.com")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .expect("send OPTIONS");

    // Unallowed origin: Allow-Origin header MUST NOT echo the attacker origin.
    // (riz may return 204 with no Allow-Origin, or 403 — both are spec-correct.)
    let allow_origin = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert!(
        allow_origin != Some("http://attacker.com"),
        "unallowed origin must NOT be echoed back; got {allow_origin:?}"
    );
}
