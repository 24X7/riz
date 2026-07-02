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
        assert!(
            tokio::time::Instant::now() < deadline,
            "server never came up"
        );
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
    assert!(
        ids.contains(&"mock"),
        "models must list the mock provider: {ids:?}"
    );
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

// ───────────────────────── Tool calling (OpenAI `tools`) ───────────────────
// The agentic contract: a client sends `tools`, the model (mock here) answers
// with `tool_calls`; the client executes and replies with a `role:"tool"`
// message; the next turn produces a final text answer. All offline via mock.

fn order_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "lookup_order",
            "description": "Look up an order by id",
            "parameters": {
                "type": "object",
                "properties": { "order_id": { "type": "string" } },
                "required": ["order_id"]
            }
        }
    })
}

#[tokio::test]
async fn chat_completions_with_tools_returns_tool_calls() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "where is order 42?"}],
            "tools": [order_tool()]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = &body["choices"][0]["message"];
    assert_eq!(
        body["choices"][0]["finish_reason"], "tool_calls",
        "tools in the request must elicit a tool_calls turn: {body}"
    );
    assert_eq!(msg["role"], "assistant");
    assert!(
        msg["content"].is_null(),
        "assistant tool_calls message carries content: null: {body}"
    );
    let call = &msg["tool_calls"][0];
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "lookup_order");
    assert!(
        call["id"].as_str().unwrap().starts_with("call_"),
        "tool call ids use the call_ prefix: {body}"
    );
    // `arguments` is a JSON-encoded STRING per the OpenAI wire format.
    let args = call["function"]["arguments"].as_str().unwrap();
    let _: serde_json::Value = serde_json::from_str(args).expect("arguments is a JSON string");
}

#[tokio::test]
async fn chat_completions_tool_result_turn_returns_text() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    // Full agent loop, turn 2: the client executed the tool and reports back.
    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [
                {"role": "user", "content": "where is order 42?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup_order", "arguments": "{\"order_id\":\"42\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "{\"status\":\"shipped\"}"}
            ],
            "tools": [order_tool()]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["finish_reason"], "stop",
        "after a tool result the turn must complete with text: {body}"
    );
    let content = body["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(
        content.contains("shipped"),
        "final answer must incorporate the tool result: {content}"
    );
}

#[tokio::test]
async fn chat_completions_tool_choice_none_returns_text() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "just chat"}],
            "tools": [order_tool()],
            "tool_choice": "none"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert!(
        body["choices"][0]["message"]["tool_calls"].is_null(),
        "tool_choice: none must suppress tool calls: {body}"
    );
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("just chat"));
}

#[tokio::test]
async fn streaming_with_tools_emits_tool_call_chunks() {
    let addr = boot(GATEWAY_CFG).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "where is order 42?"}],
            "tools": [order_tool()],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"tool_calls\""),
        "streamed tool-call turn must carry a tool_calls delta; got: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"tool_calls\""),
        "streamed tool-call turn must finish with tool_calls; got: {body}"
    );
    assert!(body.contains("[DONE]"), "must end with [DONE]; got: {body}");
}

// ───────────────────────── Bearer gating (money endpoints) ─────────────────
// The gateway endpoints spend real provider budget — when a bearer token is
// configured they MUST 401 without it, exactly like the rest of /_riz/*.

const GATEWAY_CFG_WITH_BEARER: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[auth]
bearer_token = "gw-sekrit"

[gateway]
default_provider = "mock"
fallback_chain = ["mock"]

[gateway.providers.mock]
kind = "mock"
"#;

#[tokio::test]
async fn gateway_endpoints_return_401_without_token_when_configured() {
    let addr = boot(GATEWAY_CFG_WITH_BEARER).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;
    let client = reqwest::Client::new();

    let chat = client
        .post(format!("{base}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status(), 401, "chat/completions must be gated");

    let embeddings = client
        .post(format!("{base}/_riz/v1/embeddings"))
        .json(&serde_json::json!({"model": "mock", "input": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(embeddings.status(), 401, "embeddings must be gated");

    let models = client
        .get(format!("{base}/_riz/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), 401, "models must be gated");

    let usage = client
        .get(format!("{base}/_riz/v1/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(usage.status(), 401, "usage must be gated");

    // Wrong token is as unauthorized as no token.
    let wrong = client
        .get(format!("{base}/_riz/v1/models"))
        .header("authorization", "Bearer wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401, "wrong token must be rejected");
}

#[tokio::test]
async fn gateway_accepts_the_configured_bearer() {
    let addr = boot(GATEWAY_CFG_WITH_BEARER).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/_riz/v1/chat/completions"))
        .header("authorization", "Bearer gw-sekrit")
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
}

#[tokio::test]
async fn cache_invalidate_is_bearer_gated() {
    let addr = boot(GATEWAY_CFG_WITH_BEARER).await;
    let base = format!("http://{addr}");
    wait_ready(&base).await;
    let client = reqwest::Client::new();

    let no_token = client
        .post(format!("{base}/cache/invalidate"))
        .json(&serde_json::json!({"prefix": "/"}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), 401, "cache flush must be gated");

    let with_token = client
        .post(format!("{base}/cache/invalidate"))
        .header("authorization", "Bearer gw-sekrit")
        .json(&serde_json::json!({"prefix": "/"}))
        .send()
        .await
        .unwrap();
    assert_eq!(with_token.status(), 200);
}
