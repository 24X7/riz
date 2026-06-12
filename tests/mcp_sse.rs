//! MCP Streamable HTTP — SSE transport (v1 roadmap #11, spec 2025-11-25).
//!
//! POST with `Accept: text/event-stream` answers as an SSE stream carrying
//! the JSON-RPC response as an `event: message`. GET opens the (currently
//! quiet) server-initiated channel. `Mcp-Session-Id` is issued on initialize
//! and echoed when the client supplies one. Notifications-only POSTs return
//! 202 Accepted per the Streamable HTTP spec.

use std::net::SocketAddr;
use std::sync::Arc;

use riz::config::{Config, FunctionConfig, RouteSpec, RuntimeKind};

fn base_cfg(routes: Vec<(&str, &str)>) -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from("./echo.ts"),
        timeout_ms: 5000,
        integration_timeout_ms: 30000,
        stage_variables: Default::default(),
        cache_ttl_secs: None,
        concurrency: 1,
        routes: routes
            .into_iter()
            .map(|(m, p)| RouteSpec {
                path: p.into(),
                method: m.into(),
            })
            .collect(),
        cors: None,
        authorizer: None,
        memory_mb: None,
        cpu_time_secs: None,
        allowed_paths: None,
        mcp: None,
    }
}

async fn make_state(bearer: Option<&str>) -> Arc<riz::state::AppState> {
    let mut config = Config::default();
    config.auth.bearer_token = bearer.map(str::to_string);
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_mcp",
            vec!["POST /_riz/mcp".into()],
            "$default",
        ))
        .await;
    riz_state
        .register(riz::state::FunctionState::user(
            "echo",
            base_cfg(vec![("GET", "/echo")]),
            "$default",
            0,
        ))
        .await;

    let mcp = Arc::new(riz::system::mcp::McpHandler::new(
        riz_state.clone(),
        bearer.map(str::to_string),
    ));
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> =
        vec![mcp.clone() as Arc<dyn riz::runtime::LambdaHandler>];
    let router_arc = Arc::new(riz::router::Router::new(handlers.clone()));
    mcp.set_router(router_arc).await;

    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(handlers)),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
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

fn tools_list_req() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
}

/// Parse the data payloads out of an SSE body.
fn sse_data_payloads(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).expect("SSE data line is JSON"))
        .collect()
}

// ───────────────────────────── POST + SSE ────────────────────────────────

#[tokio::test]
async fn post_with_sse_accept_streams_the_response() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "application/json, text/event-stream")
        .json(&tools_list_req())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/event-stream"), "{ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("event: message"), "{body}");
    let payloads = sse_data_payloads(&body);
    assert_eq!(payloads.len(), 1, "{body}");
    let tools = payloads[0]["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "echo"), "{body}");
}

#[tokio::test]
async fn post_without_sse_accept_returns_plain_json() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .json(&tools_list_req())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("application/json"), "{ct}");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["result"]["tools"].is_array(), "{body}");
}

#[tokio::test]
async fn sse_post_with_wrong_bearer_is_401_not_a_stream() {
    let addr = serve(make_state(Some("sekrit")).await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .header("authorization", "Bearer wrong")
        .json(&tools_list_req())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(!ct.contains("text/event-stream"), "401 must not be SSE: {ct}");
}

// ───────────────────────────── GET channel ───────────────────────────────

#[tokio::test]
async fn get_with_sse_accept_opens_the_stream() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/event-stream"), "{ct}");
    // The stream opens with a comment frame and then stays alive — read the
    // first chunk only (don't wait for the stream to end; it doesn't).
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("first SSE frame within 5s")
        .expect("stream yields a frame")
        .expect("frame reads");
    let text = String::from_utf8_lossy(&first);
    assert!(text.starts_with(':'), "first frame is a comment: {text}");
}

#[tokio::test]
async fn get_without_sse_accept_keeps_the_405_contract() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/_riz/mcp"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

#[tokio::test]
async fn get_sse_with_wrong_bearer_is_401() {
    let addr = serve(make_state(Some("sekrit")).await).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .header("authorization", "Bearer wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ───────────────────────────── Sessions ──────────────────────────────────

#[tokio::test]
async fn initialize_over_sse_is_issued_a_session_id() {
    let addr = serve(make_state(None).await).await;
    let init = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-11-25","capabilities":{},
                  "clientInfo":{"name":"t","version":"0"}}
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .expect("initialize response carries Mcp-Session-Id")
        .to_str()
        .unwrap();
    assert!(!sid.is_empty());
}

#[tokio::test]
async fn client_supplied_session_id_is_echoed() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .header("mcp-session-id", "sess-abc-123")
        .json(&tools_list_req())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("mcp-session-id").unwrap(),
        "sess-abc-123"
    );
}

#[tokio::test]
async fn delete_terminates_the_session_with_204() {
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .delete(format!("http://{addr}/_riz/mcp"))
        .header("mcp-session-id", "sess-abc-123")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

// ───────────────────────────── Notifications ─────────────────────────────

#[tokio::test]
async fn notifications_only_post_returns_202_accepted() {
    // Streamable HTTP spec: a POST containing only notifications (no `id`)
    // MUST be answered with 202 Accepted.
    let addr = serve(make_state(None).await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
}
