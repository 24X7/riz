//! WebSocket @connections management API end-to-end — Slice M.
//!
//! Wave 1 ships REST endpoints under `/_riz/connections/{id}` for
//! inspecting and controlling live WS connections:
//!   - GET    → connection metadata (function name, age, etc.)
//!   - POST   → push a message to the connected client
//!   - DELETE → close the connection
//!
//! `tests/wave_1_acceptance.rs::connections_get_inspects_connection` and
//! `connections_delete_closes_connection` only assert the handler TYPE
//! exists. POST is covered end-to-end by `tests/websocket_integration.rs`.
//! This file fills the GET + DELETE gap end-to-end.
//!
//! Two tests, both Bun-only (any WS runtime would do; only Bun has a
//! shipped WS adapter today).
//!
//! Run: `cargo nextest run --test middleware_ws_management`

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

const CHAT_HANDLER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/chat-handler/index.ts"
);

async fn boot_ws_server() -> SocketAddr {
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.chat]
protocol    = "websocket"
runtime     = "bun"
handler     = "{CHAT_HANDLER}"
timeout_ms  = 5000
concurrency = 4

[[function.chat.routes]]
path = "/chat"
"#
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());

    // Bind first to know the port for the test env var.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    // The chat fixture posts back to RIZ_TEST_BASE_URL — set it so the
    // child bun processes inherit a valid management URL.
    std::env::set_var(
        "RIZ_TEST_BASE_URL",
        format!("http://127.0.0.1:{}", addr.port()),
    );

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager
        .spawn_all(&config.functions, &registry, log_tx.clone())
        .await
        .unwrap();

    let ws_connections = riz::ws::ConnectionStore::new();
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = vec![Arc::new(
        riz::ws::management::ConnectionsHandler::new(ws_connections.clone(), None),
    )];
    let router = riz::router::Router::new(handlers);

    let state = Arc::new(riz::state::AppState {
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
        ws_connections,
    });

    tokio::spawn(async move {
        let app =
            riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for the server to be ready (the management API at /_riz/connections/x
    // returns 404 on missing id — used as the readiness probe).
    let probe_url = format!("http://127.0.0.1:{}/_riz/connections/probe", addr.port());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&probe_url).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "WS server did not become ready within 10s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    addr
}

/// Connect a WS client to /chat, drive the $connect + $default lifecycle
/// with a sentinel echo to confirm the connection is fully registered,
/// then call GET /_riz/connections to discover the live connection ID.
async fn connect_and_get_connection_id(
    addr: SocketAddr,
) -> (
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    String,
) {
    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");

    // Drive an echo round-trip to prove the connection is live + registered
    // in the ConnectionStore (echo handler POSTs via /_riz/connections/{id},
    // which fails if the id isn't in the store yet).
    socket
        .send(Message::Text("ping".into()))
        .await
        .expect("ws send");
    let reply = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("no echo within 3s")
        .expect("stream ended")
        .expect("ws read err");
    match reply {
        Message::Text(s) => assert_eq!(s, "echo: ping", "echo handshake failed"),
        other => panic!("expected text frame, got {other:?}"),
    }

    // The connection is registered. Discover its ID via the list endpoint.
    let list_url = format!("http://{addr}/_riz/connections");
    let resp = reqwest::get(&list_url).await.expect("GET /_riz/connections");
    assert_eq!(
        resp.status(),
        200,
        "list endpoint must return 200; got {}",
        resp.status()
    );
    let summaries: Vec<serde_json::Value> = resp.json().await.expect("list returns JSON array");
    assert!(
        !summaries.is_empty(),
        "list must contain at least the test's own connection"
    );
    let id = summaries[0]["connectionId"]
        .as_str()
        .expect("connectionId must be a string")
        .to_string();
    (socket, id)
}

#[tokio::test]
async fn ws_list_endpoint_includes_live_connection() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let addr = boot_ws_server().await;
    let (_socket, id) = connect_and_get_connection_id(addr).await;
    assert!(!id.is_empty(), "list must yield a non-empty connection id");
}

#[tokio::test]
async fn ws_get_connection_metadata_e2e() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let addr = boot_ws_server().await;
    let (_socket, id) = connect_and_get_connection_id(addr).await;

    let url = format!("http://{addr}/_riz/connections/{id}");
    let resp = reqwest::get(&url).await.expect("GET single connection");
    assert_eq!(resp.status(), 200, "GET on live id must return 200");
    let body: serde_json::Value = resp.json().await.expect("info returns JSON");

    assert_eq!(
        body["connectionId"].as_str(),
        Some(id.as_str()),
        "info.connectionId must match the queried id; body = {body}"
    );
    assert_eq!(
        body["function"].as_str(),
        Some("chat"),
        "info.function must be the riz.toml function name; body = {body}"
    );
    assert!(
        body["connectedAgeSecs"].is_number(),
        "info.connectedAgeSecs must be a number; body = {body}"
    );
}

#[tokio::test]
async fn ws_delete_closes_live_client_connection() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let addr = boot_ws_server().await;
    let (mut socket, id) = connect_and_get_connection_id(addr).await;

    // Hit DELETE — server-side close.
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/_riz/connections/{id}");
    let resp = client.delete(&url).send().await.expect("DELETE");
    assert_eq!(
        resp.status().as_u16(),
        204,
        "DELETE on live id must return 204; got {}",
        resp.status()
    );

    // The client should observe a Close frame (or the stream end) shortly.
    let next = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("server didn't close connection within 3s");
    match next {
        Some(Ok(Message::Close(_))) | None => { /* both signal a clean server close */ }
        Some(Ok(other)) => panic!("expected Close frame, got {other:?}"),
        Some(Err(e)) => {
            // Connection-reset is also a valid signal that server closed.
            eprintln!("post-DELETE stream returned err: {e} — treating as close");
        }
    }
}

#[tokio::test]
async fn ws_get_unknown_connection_returns_404() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let addr = boot_ws_server().await;

    let url = format!("http://{addr}/_riz/connections/nonexistent-id");
    let resp = reqwest::get(&url).await.expect("send");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "GET on missing connection id must return 404 (got {})",
        resp.status()
    );
}

#[tokio::test]
async fn ws_delete_unknown_connection_returns_404() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let addr = boot_ws_server().await;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/_riz/connections/nonexistent-id");
    let resp = client.delete(&url).send().await.expect("send");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "DELETE on missing connection id must return 404 (got {})",
        resp.status()
    );
}
