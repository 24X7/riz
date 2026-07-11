//! Wave 4.5 — Bearer-token auth on `/_riz/*` acceptance criteria.

use std::sync::Arc;

fn make_config_with_token(token: &str) -> riz::config::Config {
    let toml_str = format!(
        r#"
[auth]
bearer_token = "{token}"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#
    );
    toml::from_str(&toml_str).expect("[auth] bearer_token must parse")
}

fn make_event_with_token(
    method: &str,
    path: &str,
    token: &str,
) -> riz::gateway::ApiGatewayV2httpRequest {
    let mut e = riz::test_helpers::make_event(method, path);
    e.headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    e
}

/// AC1: [auth] bearer_token config block parses correctly.
/// AC4: /_riz/health stays open regardless of bearer_token.
#[test]
fn riz_health_open_without_auth() {
    let config = make_config_with_token("secret-token");
    // Token was parsed
    assert_eq!(
        config.auth.bearer_token.as_deref(),
        Some("secret-token"),
        "[auth] bearer_token must be parsed from config"
    );
    // Health handler does not enforce auth — test via direct invocation
    let s = Arc::new(riz::state::RizState::new());
    let h = riz::system::health::HealthHandler::new(s);
    // No auth header → should still get 200
    let event = riz::test_helpers::make_event("GET", "/_riz/health");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 200,
        "/_riz/health must return 200 with no auth header"
    );
}

/// AC2+3: When token set, /_riz/metrics returns 401 without auth.
#[test]
fn riz_metrics_returns_401_without_auth_when_token_configured() {
    let config = make_config_with_token("secret-token");
    let bearer = config.effective_bearer_token();
    assert_eq!(bearer.as_deref(), Some("secret-token"));

    let s = Arc::new(riz::state::RizState::new());
    let h = riz::system::metrics::MetricsHandler::new(
        s.clone(),
        Arc::new(riz::process::ProcessManager::new(s)),
        bearer,
        true,
    );
    let event = riz::test_helpers::make_event("GET", "/_riz/metrics");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 401,
        "/_riz/metrics must return 401 with no auth header when token configured"
    );
}

/// AC3: /_riz/metrics returns 200 with correct Bearer token.
#[test]
fn riz_metrics_returns_200_with_correct_bearer_token() {
    let config = make_config_with_token("secret-token");
    let bearer = config.effective_bearer_token();

    let s = Arc::new(riz::state::RizState::new());
    let h = riz::system::metrics::MetricsHandler::new(
        s.clone(),
        Arc::new(riz::process::ProcessManager::new(s)),
        bearer,
        true,
    );
    let event = make_event_with_token("GET", "/_riz/metrics", "secret-token");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 200,
        "/_riz/metrics must return 200 with correct Bearer token"
    );
}

/// AC3: /_riz/registry requires bearer auth when token configured.
#[test]
fn riz_registry_requires_bearer_auth() {
    let config = make_config_with_token("secret-token");
    let bearer = config.effective_bearer_token();

    let s = Arc::new(riz::state::RizState::new());
    let h = riz::system::registry::RegistryHandler::new(s, bearer);

    let rt = tokio::runtime::Runtime::new().unwrap();

    // No auth → 401
    let event_no_auth = riz::test_helpers::make_event("GET", "/_riz/registry");
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event_no_auth).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 401,
        "/_riz/registry must return 401 with no auth header"
    );

    // Correct auth → 200
    let event_auth = make_event_with_token("GET", "/_riz/registry", "secret-token");
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event_auth).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 200,
        "/_riz/registry must return 200 with correct Bearer token"
    );
}

/// AC3+6: /_riz/mcp requires bearer auth; auth check is BEFORE body parsing.
#[test]
fn riz_mcp_requires_bearer_auth() {
    let config = make_config_with_token("secret-token");
    let bearer = config.effective_bearer_token();

    let s = Arc::new(riz::state::RizState::new());
    let h = riz::system::mcp::McpHandler::new(s, bearer);

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Wrong token + malformed body → 401 (not a JSON-RPC parse error)
    let mut malformed_event = riz::test_helpers::make_event_with_body(
        "POST",
        "/_riz/mcp",
        "this is definitely not json {{{{",
    );
    malformed_event.headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer wrong-token"),
    );
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, malformed_event).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 401,
        "wrong token + malformed body must return 401 (auth check before body parsing)"
    );

    // No auth → 401
    let event_no_auth = riz::test_helpers::make_event_with_body(
        "POST",
        "/_riz/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    );
    let resp = rt
        .block_on(async { riz::runtime::LambdaHandler::invoke(&h, event_no_auth).await })
        .unwrap();
    assert_eq!(
        resp.status_code, 401,
        "/_riz/mcp must return 401 with no auth header"
    );
}
