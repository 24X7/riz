//! Layer 1 — HTTP boundary golden tests. These pin the externally observable
//! behavior of the server. Every test must still pass through any refactor.

use std::net::SocketAddr;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::config::{Config, FunctionConfig, RuntimeKind};

fn make_state_with_functions(
    functions: IndexMap<String, FunctionConfig>,
) -> Arc<riz::state::AppState> {
    let config = Config {
        functions,
        ..Default::default()
    };
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    // Build one ProcessHandler per declared function (no spawn — these tests
    // exercise the routing surface and pre-invoke body/cache paths).
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config
        .functions
        .iter()
        .map(|(name, cfg)| {
            let h = riz::runtime::process::ProcessHandler::for_function(
                name,
                cfg,
                process_manager.clone(),
            );
            Arc::new(h) as Arc<dyn riz::runtime::LambdaHandler>
        })
        .collect();
    let router = riz::router::Router::new(handlers);

    Arc::new(riz::state::AppState {
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
    })
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn health_returns_200_ok_json() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_200_when_all_pools_healthy() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/ready")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let resp = reqwest::get(format!("http://{addr}/no-such-route"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn deploy_without_auth_returns_503() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/deploy"))
        .json(&serde_json::json!({
            "lambda": "x",
            "s3_bucket": "b",
            "s3_key": "k"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn cache_invalidate_with_keys_returns_evicted_count() {
    let state = make_state_with_functions(IndexMap::new());
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/cache/invalidate"))
        .json(&serde_json::json!({"keys":["nonexistent"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["evicted"].is_number());
}

#[tokio::test]
async fn oversized_body_returns_413_for_routed_request() {
    // The 10 MB body cap is enforced inside dispatch_lambda AFTER route match.
    // A request to a path with no route gets 404 before the body is read, so
    // we register a synthetic function and target its path with an oversized body.
    let mut functions = IndexMap::new();
    functions.insert(
        "sink".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 1000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![riz::config::RouteSpec {
                path: "/sink".into(),
                method: "POST".into(),
            }],
            cors: None,
            authorizer: None,
        },
    );
    let state = make_state_with_functions(functions);
    let addr = serve(state).await;
    let client = reqwest::Client::new();
    let big_body = vec![b'x'; 11 * 1024 * 1024];
    let resp = client
        .post(format!("http://{addr}/sink"))
        .body(big_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
}

// ─── 8.5 dispatch hot path: auth-bypass skips cache ──────────────────────────

/// A request that carries an `Authorization` header must bypass the cache and
/// go straight to the handler.  Because no real handler is running in this test,
/// the response will be a 502/429/5xx from the pool machinery — the key
/// assertion is that the response is NOT a cached 200 from a prior request.
///
/// We test the auth bypass indirectly:
/// 1. Issue a GET request WITHOUT auth to a function whose process pool is
///    empty (no real bun process started) — it will return an error response.
/// 2. Manually prime the cache with a fake 200 for the same key.
/// 3. Issue a second GET WITH an Authorization header.
/// 4. Assert the second response is NOT 200 (i.e., the cache was bypassed and
///    the pool was hit again, returning the error response).
#[tokio::test]
async fn auth_bypass_skips_cache() {
    let mut functions = IndexMap::new();
    functions.insert(
        "auth-fn".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 100,
            integration_timeout_ms: 200,
            stage_variables: Default::default(),
            cache_ttl_secs: Some(60),
            concurrency: 1,
            routes: vec![riz::config::RouteSpec {
                path: "/auth-resource".into(),
                method: "GET".into(),
            }],
            cors: None,
            authorizer: None,
        },
    );
    let state = make_state_with_functions(functions);

    // Prime the cache manually with a 200 response for GET /auth-resource.
    let cache_key = riz::cache::CacheLayer::make_key("GET", "/auth-resource", "");
    let fake_resp = riz::gateway::ApiGatewayV2httpResponse {
        status_code: 200,
        headers: http::HeaderMap::new(),
        multi_value_headers: http::HeaderMap::new(),
        body: Some(riz::gateway::Body::Text("cached".to_string())),
        is_base64_encoded: false,
        cookies: vec![],
    };
    state.cache.set(cache_key, fake_resp, 60).await;

    let addr = serve(state).await;
    let client = reqwest::Client::new();

    // A request WITH Authorization must not return the cached 200.
    let resp = client
        .get(format!("http://{addr}/auth-resource"))
        .header("Authorization", "Bearer my-token")
        .send()
        .await
        .unwrap();

    // Cache was bypassed → the pool was hit (process not running) → non-200.
    assert_ne!(
        resp.status().as_u16(),
        200,
        "request with Authorization header must bypass cache (cache would return 200)"
    );
}

// ─── 8.5 dispatch hot path: integration_timeout returns 504 ──────────────────

/// Verifies that the integration timeout (outer wrapper in `ProcessHandler`)
/// returns HTTP 504, not 500 or 502, when it fires before the handler responds.
///
/// This test exercises the full HTTP layer by using bun + the echo handler
/// configured with a very short `integration_timeout_ms` (50 ms) but a long
/// `timeout_ms` (60 s). The echo handler responds quickly in normal operation,
/// but by setting `integration_timeout_ms` shorter than the handler startup
/// time on first cold-start, we can observe the 504 path.
///
/// Because cold-start time is unpredictable, we also validate the 504 status
/// code mapping at the unit level (no live process needed).
#[test]
fn integration_timeout_returns_504_unit_level() {
    // Unit-level proof: HandlerError::Timeout maps to 504 regardless of whether
    // the timeout came from integration_timeout_ms or timeout_ms. This is the
    // key contract the dispatch layer relies on.
    use riz::runtime::HandlerError;
    let err = HandlerError::Timeout(150);
    assert_eq!(
        err.status_code(),
        504,
        "HandlerError::Timeout must map to 504 (integration timeout contract)"
    );
    let resp = err.to_response();
    assert_eq!(
        resp.status_code, 504,
        "HandlerError::Timeout.to_response() must produce status_code=504"
    );
    // The response body must say something about the timeout.
    let body_str = match &resp.body {
        Some(riz::gateway::Body::Text(s)) => s.clone(),
        _ => String::new(),
    };
    assert!(
        !body_str.is_empty(),
        "504 response must have a non-empty body describing the timeout"
    );
}

/// Integration-level 504 test: gate on bun.
///
/// Uses a sleep-based lambda fixture (sleeps 500ms) with `integration_timeout_ms=200`
/// to guarantee the integration timeout fires before the handler responds.
/// Requires bun on PATH.
#[tokio::test]
async fn gateway_timeout_returns_504_for_routed_request() {
    // Skip if bun is not installed.
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("gateway_timeout_returns_504_for_routed_request: bun not on PATH — skipping");
        return;
    }

    // The sleep-lambda sleeps 500ms before responding.
    // integration_timeout_ms = 200 fires first → 504.
    let mut functions = IndexMap::new();
    functions.insert(
        "slow-fn".to_string(),
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/sleep-lambda/index.ts"
            )),
            timeout_ms: 60_000,
            integration_timeout_ms: 200, // fires before the 500ms sleep completes
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![riz::config::RouteSpec {
                path: "/slow".into(),
                method: "GET".into(),
            }],
            cors: None,
            authorizer: None,
        },
    );
    let state = make_state_with_functions(functions.clone());

    // Spawn the real bun pool.
    let registry = state.runtime_registry.clone();
    state
        .process_manager
        .spawn_all(
            &Config {
                functions,
                ..Default::default()
            }
            .functions,
            &registry,
            state.log_tx.clone(),
        )
        .await
        .expect("spawn_all must succeed");

    let addr = serve(state).await;

    // Allow bun to cold-start before firing the request that must time out.
    // This ensures the 504 comes from integration_timeout_ms, not from pool startup.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let start = std::time::Instant::now();
    let resp = reqwest::get(format!("http://{addr}/slow")).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        resp.status().as_u16(),
        504,
        "integration_timeout_ms=200 must produce 504 for 500ms sleep handler (got {} after {:?})",
        resp.status(),
        elapsed
    );

    // Must respond within integration_timeout_ms + generous CI margin.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "integration timeout must fire within 5s (got {:?})",
        elapsed
    );
}
