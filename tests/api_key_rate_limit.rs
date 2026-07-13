//! W1.1 — per-caller API keys + token-bucket rate limiting, exercised through
//! the real HTTP dispatch path (`dispatch_lambda`).
//!
//! The admission gate runs before static serving, the cache, and routing, so
//! every assertion here is deterministic without a live runtime: a rejected
//! caller never reaches a handler, and an *admitted* caller simply falls
//! through to a 404 (no route registered). The gate's 429 is distinguishable
//! from a pool load-shed 429 by its `Retry-After` header (the pool sets none).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::config::{ApiKeyEntry, Config};

fn keys(entries: &[(&str, &str, Option<u32>)]) -> IndexMap<String, ApiKeyEntry> {
    entries
        .iter()
        .map(|(name, key, rate)| {
            (
                (*name).to_string(),
                ApiKeyEntry {
                    key: (*key).to_string(),
                    rate_per_sec: *rate,
                },
            )
        })
        .collect()
}

fn make_state(api_keys: IndexMap<String, ApiKeyEntry>) -> Arc<riz::state::AppState> {
    let config = Config {
        api_keys,
        ..Default::default()
    };
    assert!(config.validate().is_ok(), "test config must be valid");
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    let rate_limiter = riz::auth::api_key::RateLimiter::from_config(&config.api_keys);
    let router = riz::router::Router::new(vec![]);

    Arc::new(riz::state::AppState {
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
        rate_limiter: tokio::sync::RwLock::new(rate_limiter),
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
async fn no_keys_configured_leaves_data_plane_open() {
    // Empty [api_keys] → open: an unauthenticated request is not gated (falls
    // through to 404 for the unrouted path, never 401).
    let addr = serve(make_state(IndexMap::new())).await;
    let resp = reqwest::get(format!("http://{addr}/anything"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "no keys → open, unrouted path is a 404");
}

#[tokio::test]
async fn missing_or_wrong_key_fails_closed_401() {
    let addr = serve(make_state(keys(&[("alice", "secret-a", Some(100))]))).await;
    let client = reqwest::Client::new();

    // No X-Api-Key at all.
    let resp = client
        .get(format!("http://{addr}/anything"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "absent key must fail closed");

    // Wrong X-Api-Key.
    let resp = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "not-the-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unknown key must fail closed");
}

#[tokio::test]
async fn valid_key_is_admitted() {
    // A valid key passes the gate; with no route registered the request falls
    // through to 404 — proving admission (not 401/429).
    let addr = serve(make_state(keys(&[("alice", "secret-a", Some(100))]))).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "secret-a")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "valid key admitted → falls through to 404"
    );
}

#[tokio::test]
async fn exhausting_bucket_yields_429_with_retry_after() {
    // rate_per_sec = 1 → one token, then empty. The second immediate request
    // is a gate 429 carrying Retry-After (a pool load-shed 429 would not).
    let addr = serve(make_state(keys(&[("alice", "a", Some(1))]))).await;
    let client = reqwest::Client::new();

    let first = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap();
    assert_eq!(
        first.status(),
        404,
        "first request spends the token, admitted"
    );

    let second = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 429, "bucket empty → rate limited");
    let retry_after = second
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok());
    assert!(
        retry_after.is_some(),
        "gate 429 must carry Retry-After (got headers {:?})",
        second.headers()
    );
    assert!(
        retry_after.unwrap().parse::<u64>().unwrap() >= 1,
        "Retry-After is a whole-second count >= 1"
    );
}

#[tokio::test]
async fn one_caller_exhausting_does_not_affect_another() {
    // Isolation: alice (rate 1) exhausts her bucket; bob (rate 1, untouched)
    // is still admitted.
    let addr = serve(make_state(keys(&[
        ("alice", "a", Some(1)),
        ("bob", "b", Some(1)),
    ])))
    .await;
    let client = reqwest::Client::new();

    // alice: spend token (404), then 429.
    let a1 = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap();
    assert_eq!(a1.status(), 404);
    let a2 = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap();
    assert_eq!(a2.status(), 429, "alice is now rate limited");

    // bob: unaffected by alice's exhaustion.
    let b1 = client
        .get(format!("http://{addr}/anything"))
        .header("x-api-key", "b")
        .send()
        .await
        .unwrap();
    assert_eq!(b1.status(), 404, "bob's bucket is independent");
}

#[tokio::test]
async fn riz_admin_plane_is_exempt_from_api_key_gate() {
    // /health is a mounted admin route, not a data-plane fallback. It must
    // stay reachable without an X-Api-Key even when keys are configured.
    let addr = serve(make_state(keys(&[("alice", "a", Some(100))]))).await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200, "/health must not require an API key");
}

#[tokio::test]
async fn riz_reserved_prefix_is_exempt_but_data_plane_is_gated() {
    // Directly exercises the `!path.starts_with("/_riz/")` gate branch: with
    // keys configured, a `/_riz/*` path is NOT 401'd (it falls through to 404
    // here — no route registered), while a data-plane path with no key IS
    // 401'd. A regression that gated `/_riz/*` would 401 metrics scrapes.
    let addr = serve(make_state(keys(&[("alice", "a", Some(100))]))).await;

    let reserved = reqwest::get(format!("http://{addr}/_riz/whatever"))
        .await
        .unwrap();
    assert_ne!(
        reserved.status(),
        401,
        "/_riz/* must be exempt from the API-key gate (got {})",
        reserved.status()
    );
    assert_eq!(
        reserved.status(),
        404,
        "unrouted /_riz/* falls through to 404"
    );

    let data_plane = reqwest::get(format!("http://{addr}/data")).await.unwrap();
    assert_eq!(
        data_plane.status(),
        401,
        "a non-/_riz/ data-plane path with no key must be gated"
    );
}

#[tokio::test]
async fn keyed_request_bypasses_the_response_cache() {
    // The cache key is method+path+query only — it does not encode the caller.
    // A request carrying X-Api-Key must therefore skip the cache, or caller B
    // could be served caller A's cached response. We prime the cache with a
    // sentinel 200 and prove a keyed request does NOT get it.
    let state = make_state(keys(&[("alice", "secret-a", Some(100))]));
    let cache_key = riz::cache::CacheLayer::make_key("GET", "/shared", "");
    let sentinel = riz::gateway::ApiGatewayV2httpResponse {
        status_code: 200,
        headers: http::HeaderMap::new(),
        multi_value_headers: http::HeaderMap::new(),
        body: Some(riz::gateway::Body::Text(
            "cached-for-someone-else".to_string(),
        )),
        is_base64_encoded: false,
        cookies: vec![],
    };
    state.cache.set(cache_key, sentinel, 60).await;

    let addr = serve(state).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/shared"))
        .header("x-api-key", "secret-a")
        .send()
        .await
        .unwrap();
    // Cache bypassed → falls through to 404 (no route). The key point: NOT the
    // cached 200 belonging to a prior request.
    assert_ne!(
        resp.status(),
        200,
        "keyed request must not be served another caller's cached response"
    );
}

/// Build an AppState with one `protocol = websocket` function mounted at
/// `/ws` and the given API keys. No pool is spawned — the handshake gate runs
/// before any upgrade/dispatch, so no runtime binary is needed.
fn make_ws_state(api_keys: IndexMap<String, ApiKeyEntry>) -> Arc<riz::state::AppState> {
    let toml = r#"
[server]
host = "127.0.0.1"

[function.ws]
protocol = "websocket"
runtime = "bun"
handler = "./does-not-exist.ts"
timeout_ms = 5000
concurrency = 1

[[function.ws.routes]]
path = "/ws"
"#;
    let mut config: Config = toml::from_str(toml).unwrap();
    config.api_keys = api_keys;
    config.validate().unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    let rate_limiter = riz::auth::api_key::RateLimiter::from_config(&config.api_keys);

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(vec![])),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(rate_limiter),
    })
}

#[tokio::test]
async fn websocket_handshake_is_gated_by_api_keys() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // A WebSocket function is a data-plane surface: with keys configured, a
    // handshake with no X-Api-Key must be rejected (F1 regression guard —
    // WS routes are mounted explicitly and previously bypassed the gate).
    let addr = serve(make_ws_state(keys(&[("alice", "secret-a", Some(100))]))).await;

    let no_key = format!("ws://{addr}/ws").into_client_request().unwrap();
    assert!(
        tokio_tungstenite::connect_async(no_key).await.is_err(),
        "keyless WS handshake must be rejected when [api_keys] is set"
    );

    // A valid key passes the gate → the handshake upgrades (101). The socket
    // may close immediately after ($connect has no worker), but the upgrade
    // itself succeeding proves the gate admitted the caller.
    let mut with_key = format!("ws://{addr}/ws").into_client_request().unwrap();
    with_key
        .headers_mut()
        .insert("x-api-key", "secret-a".parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(with_key).await.is_ok(),
        "a valid key must be admitted through the WS handshake"
    );
}

#[tokio::test]
async fn websocket_handshake_open_when_no_keys() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    // No keys configured → the WS handshake is open (unchanged behavior).
    let addr = serve(make_ws_state(IndexMap::new())).await;
    let req = format!("ws://{addr}/ws").into_client_request().unwrap();
    assert!(
        tokio_tungstenite::connect_async(req).await.is_ok(),
        "with no keys, the WS handshake stays open"
    );
}

#[tokio::test]
async fn admission_counters_increment() {
    let state = make_state(keys(&[("alice", "a", Some(1))]));
    let riz_state = state.riz_state.clone();
    let addr = serve(state).await;
    let client = reqwest::Client::new();

    // One 401 (no key) and one 429 (exhaust the single token then exceed).
    let _ = client.get(format!("http://{addr}/x")).send().await.unwrap(); // 401
    let _ = client
        .get(format!("http://{addr}/x"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap(); // 404, spends token
    let _ = client
        .get(format!("http://{addr}/x"))
        .header("x-api-key", "a")
        .send()
        .await
        .unwrap(); // 429

    assert_eq!(
        riz_state.api_key_rejected.load(Ordering::Relaxed),
        1,
        "one unauthorized rejection counted"
    );
    assert_eq!(
        riz_state.rate_limited.load(Ordering::Relaxed),
        1,
        "one rate-limit rejection counted"
    );
}
