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
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
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
        metrics,
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

#[tokio::test]
async fn streaming_request_is_rejected_until_supported() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
