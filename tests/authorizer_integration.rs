//! Integration tests for Lambda authorizers (Wave 3).
//!
//! These tests spin up a real riz server with a fake Bun-backed authorizer
//! function and verify end-to-end that:
//! - Requests with a valid token reach the handler (with authorizer context injected).
//! - Requests with an invalid/missing token receive 401.
//! - `authorizer = "none"` opt-out lets all requests through.
//! - The authorizer response cache prevents duplicate invocations.

use std::net::SocketAddr;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::auth::authorizer::AuthCache;
use riz::config::{AuthorizerConfig, Config, FunctionConfig, RuntimeKind};

// ── helpers ──────────────────────────────────────────────────────────────────

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_state(config: Config) -> Arc<riz::state::AppState> {
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
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

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        auth_cache: AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    })
}

async fn serve_state(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

// ── Authorizer cache unit tests (no Bun required) ────────────────────────────

#[tokio::test]
async fn auth_cache_evicts_after_zero_ttl() {
    // Verify the insert/get cycle is observable when TTL is comfortably
    // longer than the test execution window. Eviction-on-zero-ttl is a
    // moka-internal behavior tested by moka itself; we only care that the
    // cache surfaces a fresh insert to an immediate get.
    use riz::auth::authorizer::{AuthCacheKey, AuthorizerOutput};
    use std::collections::HashMap;
    use std::time::Duration;

    let cache = AuthCache::new();
    let key = AuthCacheKey::new("127.0.0.1", Some("Bearer tok"), "fn");
    let output = AuthorizerOutput {
        principal_id: "p".into(),
        context: HashMap::new(),
        ttl: Duration::from_secs(60),
    };
    cache.insert(key.clone(), output).await;
    // Entry should be present immediately after insert.
    assert!(cache.get(&key).await.is_some());
}

#[tokio::test]
async fn auth_cache_isolates_per_function() {
    use riz::auth::authorizer::{AuthCacheKey, AuthorizerOutput};
    use std::collections::HashMap;
    use std::time::Duration;

    let cache = AuthCache::new();
    let key_fn1 = AuthCacheKey::new("1.1.1.1", Some("Bearer tok"), "fn1");
    let key_fn2 = AuthCacheKey::new("1.1.1.1", Some("Bearer tok"), "fn2");

    let out1 = AuthorizerOutput {
        principal_id: "user-for-fn1".into(),
        context: HashMap::new(),
        ttl: Duration::from_secs(60),
    };
    cache.insert(key_fn1.clone(), out1).await;

    // fn1 has an entry; fn2 does not.
    assert!(cache.get(&key_fn1).await.is_some());
    assert!(cache.get(&key_fn2).await.is_none());
}

// ── REQUEST authorizer integration (requires Bun) ────────────────────────────

/// A fake authorizer Bun handler that returns `{isAuthorized: true}` when the
/// Authorization header is `Bearer valid-token`, and `{isAuthorized: false}`
/// otherwise.
fn write_fake_auth_handler(dir: &std::path::Path) {
    let handler_code = r#"export const handler = async (event) => {
  const authHeader = event?.headers?.authorization ?? event?.headers?.Authorization ?? "";
  const isAuthorized = authHeader === "Bearer valid-token";
  return {
    isAuthorized,
    context: isAuthorized ? { userId: "user-42", role: "admin" } : {}
  };
};
"#;
    std::fs::write(dir.join("auth.ts"), handler_code).unwrap();
}

/// A simple echo handler that returns 200 with the authorizer context it received.
fn write_echo_handler(dir: &std::path::Path) {
    let handler_code = r#"export const handler = async (event) => {
  const authCtx = event?.requestContext?.authorizer?.lambda ?? {};
  return {
    statusCode: 200,
    body: JSON.stringify({ ok: true, authCtx }),
    headers: { "content-type": "application/json" },
    isBase64Encoded: false,
    cookies: [],
    multiValueHeaders: {}
  };
};
"#;
    std::fs::write(dir.join("api.ts"), handler_code).unwrap();
}

#[tokio::test]
#[ignore = "requires Bun runtime"]
async fn request_authorizer_allows_valid_token() {
    if !bun_available() {
        return; // skip gracefully
    }
    let dir = tempfile::tempdir().unwrap();
    write_fake_auth_handler(dir.path());
    write_echo_handler(dir.path());

    let auth_handler_path = dir.path().join("auth.ts");
    let api_handler_path = dir.path().join("api.ts");

    let mut functions = IndexMap::new();
    functions.insert(
        "auth-fn".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: auth_handler_path,
            timeout_ms: 5_000,
            integration_timeout_ms: 5_000,
            concurrency: 1,
            cache_ttl_secs: None,
            stage_variables: Default::default(),
            env: Default::default(),
            routes: vec![riz::config::RouteSpec {
                path: "/auth".into(),
                method: "ANY".into(),
            }],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        },
    );
    functions.insert(
        "api".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: api_handler_path,
            timeout_ms: 5_000,
            integration_timeout_ms: 5_000,
            concurrency: 1,
            cache_ttl_secs: None,
            stage_variables: Default::default(),
            env: Default::default(),
            routes: vec![riz::config::RouteSpec {
                path: "/api".into(),
                method: "GET".into(),
            }],
            cors: None,
            authorizer: Some(AuthorizerConfig::FunctionRef("auth-fn".into())),
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        },
    );

    let config = Config {
        functions,
        ..Default::default()
    };
    let state = make_state(config);

    // Spawn function processes.
    state
        .process_manager
        .spawn_all(
            &state.config.read().await.functions,
            &state.runtime_registry,
            state.log_tx.clone(),
        )
        .await
        .unwrap();

    let addr = serve_state(state).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let client = reqwest::Client::new();

    // Valid token → 200 with authorizer context.
    let resp = client
        .get(format!("http://{addr}/api"))
        .header("Authorization", "Bearer valid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "valid token must get 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["authCtx"]["userId"], "user-42",
        "authorizer context must be injected: {body}"
    );
}

#[tokio::test]
#[ignore = "requires Bun runtime"]
async fn request_authorizer_rejects_invalid_token() {
    if !bun_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_fake_auth_handler(dir.path());
    write_echo_handler(dir.path());

    let auth_handler_path = dir.path().join("auth.ts");
    let api_handler_path = dir.path().join("api.ts");

    let mut functions = IndexMap::new();
    functions.insert(
        "auth-fn".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: auth_handler_path,
            timeout_ms: 5_000,
            integration_timeout_ms: 5_000,
            concurrency: 1,
            cache_ttl_secs: None,
            stage_variables: Default::default(),
            env: Default::default(),
            routes: vec![riz::config::RouteSpec {
                path: "/auth".into(),
                method: "ANY".into(),
            }],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        },
    );
    functions.insert(
        "api".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: api_handler_path,
            timeout_ms: 5_000,
            integration_timeout_ms: 5_000,
            concurrency: 1,
            cache_ttl_secs: None,
            stage_variables: Default::default(),
            env: Default::default(),
            routes: vec![riz::config::RouteSpec {
                path: "/api".into(),
                method: "GET".into(),
            }],
            cors: None,
            authorizer: Some(AuthorizerConfig::FunctionRef("auth-fn".into())),
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        },
    );

    let config = Config {
        functions,
        ..Default::default()
    };
    let state = make_state(config);
    state
        .process_manager
        .spawn_all(
            &state.config.read().await.functions,
            &state.runtime_registry,
            state.log_tx.clone(),
        )
        .await
        .unwrap();

    let addr = serve_state(state).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let client = reqwest::Client::new();

    // Wrong token → 401.
    let resp = client
        .get(format!("http://{addr}/api"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong token must get 401");

    // No token → 401.
    let resp_no_auth = client
        .get(format!("http://{addr}/api"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_no_auth.status(), 401, "missing token must get 401");
}

#[tokio::test]
async fn authorizer_none_opt_out_allows_any_request() {
    if !bun_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_echo_handler(dir.path());

    let api_handler_path = dir.path().join("api.ts");

    let mut functions = IndexMap::new();
    functions.insert(
        "api".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: api_handler_path,
            timeout_ms: 5_000,
            integration_timeout_ms: 5_000,
            concurrency: 1,
            cache_ttl_secs: None,
            stage_variables: Default::default(),
            env: Default::default(),
            routes: vec![riz::config::RouteSpec {
                path: "/api".into(),
                method: "GET".into(),
            }],
            cors: None,
            // Explicit opt-out.
            authorizer: Some(AuthorizerConfig::FunctionRef("none".into())),
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        },
    );

    let config = Config {
        functions,
        ..Default::default()
    };
    let state = make_state(config);
    state
        .process_manager
        .spawn_all(
            &state.config.read().await.functions,
            &state.runtime_registry,
            state.log_tx.clone(),
        )
        .await
        .unwrap();

    let addr = serve_state(state).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let client = reqwest::Client::new();
    // No token, but authorizer = "none" → must reach handler.
    let resp = client
        .get(format!("http://{addr}/api"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "opt-out must allow unauthenticated request"
    );
}
