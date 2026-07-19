//! REQUEST authorizer middleware — Slice J.
//!
//! Two tests, both Bun-only (authorizer middleware is runtime-agnostic
//! at the call site — the protected handler's runtime doesn't affect
//! the auth flow).
//!
//! 1. allow path: a REQUEST authorizer returns `{isAuthorized: true,
//!    context: {principalId: "u42", tier: "gold"}}`. riz must invoke
//!    the protected handler with `event.requestContext.authorizer.fields`
//!    populated.
//!
//! 2. deny path: the authorizer returns `{isAuthorized: false}`. riz
//!    must return 401 Unauthorized WITHOUT invoking the protected
//!    handler (proven by the absence of the x-riz-echo header).
//!
//! Run: `cargo nextest run --test middleware_request_authorizer`

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
const AUTH_ALLOW_BUN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/lambdas/auth-allow-bun/index.handler"
);
const AUTH_DENY_BUN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/lambdas/auth-deny-bun/index.handler"
);

#[tokio::test]
async fn request_authorizer_allow_populates_handler_context() {
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
authorizer = "auth-allow"

[[function.echo-bun.routes]]
path = "/protected"
method = "GET"

[function.auth-allow]
runtime = "bun"
handler = "{AUTH_ALLOW_BUN}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.auth-allow.routes]]
path = "/_authorizer/auth-allow"
method = "POST"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let client = reqwest::Client::new();
    wait_for_ready(&client, &format!("http://{addr}/protected")).await;

    let resp = client
        .get(format!("http://{addr}/protected"))
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        200,
        "allow path must return 200 from protected handler"
    );

    let body: serde_json::Value = resp.json().await.expect("json");
    let authorizer = &body["authorizer"];
    assert!(
        !authorizer.is_null(),
        "event.requestContext.authorizer must be populated; body = {body}"
    );

    // The authorizer fields are nested under the AWS shape — search both
    // `authorizer.fields.principalId` (the documented v2 shape) and
    // `authorizer.principalId` (some adapters flatten).
    let principal = authorizer
        .pointer("/fields/principalId")
        .or_else(|| authorizer.get("principalId"))
        .or_else(|| authorizer.pointer("/lambda/principalId"));
    assert_eq!(
        principal.and_then(|v| v.as_str()),
        Some("u42"),
        "authorizer must inject principalId = \"u42\"; full authorizer = {authorizer}"
    );

    let tier = authorizer
        .pointer("/fields/tier")
        .or_else(|| authorizer.get("tier"))
        .or_else(|| authorizer.pointer("/lambda/tier"));
    assert_eq!(
        tier.and_then(|v| v.as_str()),
        Some("gold"),
        "authorizer context.tier must be \"gold\"; full authorizer = {authorizer}"
    );
}

#[tokio::test]
async fn request_authorizer_deny_returns_401_without_invoking_handler() {
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
authorizer = "auth-deny"

[[function.echo-bun.routes]]
path = "/protected"
method = "GET"

[function.auth-deny]
runtime = "bun"
handler = "{AUTH_DENY_BUN}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.auth-deny.routes]]
path = "/_authorizer/auth-deny"
method = "POST"
"#
    );
    let addr = boot_riz(&config_toml).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/protected"))
        .send()
        .await
        .expect("send");

    assert_eq!(
        resp.status(),
        401,
        "deny path must return 401 from authorizer middleware"
    );

    // The protected handler always emits x-riz-echo: ok. Its absence proves
    // dispatch short-circuited at the authorizer before the handler ran.
    assert!(
        resp.headers().get("x-riz-echo").is_none(),
        "deny path must NOT invoke the protected handler (x-riz-echo present)"
    );
}
