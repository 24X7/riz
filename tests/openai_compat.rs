//! OpenAI-compatible endpoint (/_riz/v1/*) e2e, backed by the mock provider.
//! No API key, no network — exercises the real gateway + HTTP wiring.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

async fn boot(config_toml: &str) -> SocketAddr {
    let config: riz::config::Config = toml::from_str(config_toml).expect("toml parses");
    config.validate().expect("config validates");

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));

    let router = riz::router::Router::new(vec![]);
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    bound
}

const GATEWAY_CFG: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[gateway]
default_provider = "mock"
fallback_chain = ["mock"]

[gateway.providers.mock]
kind = "mock"
"#;

async fn wait_ready(base: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(format!("{base}/ready")).await.is_ok() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "server never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn chat_completions_returns_openai_shape() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello riz"}],
            "stream": false
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("hello riz"));
    assert!(body["usage"]["total_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn models_lists_configured_providers() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/_riz/v1/models"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["object"], "list");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"mock"), "models must list the mock provider: {ids:?}");
}

const BUDGET_CFG: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[gateway]
default_provider = "mock"
budget_usd = 0.0

[gateway.providers.mock]
kind = "mock"
"#;

#[tokio::test]
async fn usage_endpoint_reports_cost_after_a_call() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let _ = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    let usage: serde_json::Value = reqwest::get(format!("{base}/_riz/v1/usage"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        usage["total_cost_usd"].as_f64().unwrap() > 0.0,
        "usage must report cost after a call: {usage}"
    );
    assert!(usage["providers"]["mock"]["requests"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn budget_zero_rejects_with_412() {
    let addr = boot(BUDGET_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 412, "budget_usd = 0 must reject with 412");
}

#[tokio::test]
async fn embeddings_returns_openai_shape() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let body: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/embeddings"))
        .json(&serde_json::json!({ "model": "mock", "input": ["alpha", "beta"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "one embedding per input");
    assert_eq!(data[0]["object"], "embedding");
    assert_eq!(data[0]["index"], 0);
    assert!(
        data[0]["embedding"].as_array().unwrap().len() >= 8,
        "embedding vector must be non-trivial"
    );
    // Deterministic: same input → same vector.
    let again: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/embeddings"))
        .json(&serde_json::json!({ "model": "mock", "input": "alpha" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["data"][0]["embedding"], data[0]["embedding"]);
}

#[tokio::test]
async fn streaming_returns_openai_sse_chunks() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "hello stream"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ctype = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(
        ctype.contains("text/event-stream"),
        "streaming response must be SSE; got content-type {ctype}"
    );
    let body = resp.text().await.unwrap();
    // OpenAI streaming wire format: chunk objects then a [DONE] sentinel.
    assert!(
        body.contains("chat.completion.chunk"),
        "must emit chat.completion.chunk objects; got: {body}"
    );
    assert!(
        body.contains("hello") && body.contains("stream"),
        "streamed content must include the echoed prompt words; got: {body}"
    );
    // The role delta opens the stream and a stop finish_reason closes it.
    assert!(body.contains("\"role\":\"assistant\""), "got: {body}");
    assert!(body.contains("\"finish_reason\":\"stop\""), "got: {body}");
    assert!(body.contains("[DONE]"), "must end with [DONE]; got: {body}");
}
