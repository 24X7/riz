//! End-to-end WebSocket tests. Spin up the server with a test-fixture chat
//! handler, connect with tokio-tungstenite, and drive the socket directly:
//! echo roundtrip via the @connections management path, and the inbound
//! frame-size cap (AWS parity: 32 KB/frame, 128 KB/message).
//!
//! Requires `bun` on PATH.

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Start a riz server with the chat-handler fixture mounted at /chat.
/// Returns the bound address; the server task runs until the test exits.
async fn start_chat_server() -> SocketAddr {
    let handler_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/chat-handler/index.ts"
    );

    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.chat]
protocol    = "websocket"
runtime     = "bun"
handler     = "{handler}"
timeout_ms  = 5000
concurrency = 4

[[function.chat.routes]]
path = "/chat"
"#,
        handler = handler_path
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    config.validate().unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());

    // Bind first so we know the port before spawning child processes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    // Set the env var BEFORE spawn_all so the Bun child processes inherit it.
    // The fixture handler reads this to POST back to our dynamic port.
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
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections,
    });

    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    // Give the server a moment to be ready.
    tokio::time::sleep(Duration::from_millis(300)).await;

    addr
}

#[tokio::test]
async fn websocket_echo_roundtrip() {
    let addr = start_chat_server().await;

    // Connect via WebSocket.
    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect should succeed");

    // Send a message.
    socket
        .send(Message::Text("hello riz".into()))
        .await
        .unwrap();

    // Wait for the echoed reply (the handler POSTs back via @connections).
    let reply = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("no reply within 2s")
        .expect("stream ended")
        .expect("ws read error");

    match reply {
        Message::Text(s) => assert_eq!(s, "echo: hello riz"),
        other => panic!("expected text frame, got {other:?}"),
    }

    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn websocket_oversized_inbound_frame_closes_connection() {
    let addr = start_chat_server().await;

    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect should succeed");

    // Sanity: the connection works for a normal-sized frame first.
    socket.send(Message::Text("ping".into())).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("no reply within 2s")
        .expect("stream ended")
        .expect("ws read error");
    assert_eq!(reply, Message::Text("echo: ping".into()));

    // A 200 KiB single-frame message exceeds both inbound caps
    // (32 KiB/frame, 128 KiB/message — AWS API Gateway WebSocket quotas).
    // The server must tear the connection down without ever dispatching the
    // payload; the client sees a Close frame or a dropped connection, never
    // an echo.
    let oversized = "x".repeat(200 * 1024);
    // The send itself may or may not error depending on how fast the server
    // resets; either is acceptable.
    let _ = socket.send(Message::Text(oversized)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let next = tokio::time::timeout_at(deadline, socket.next()).await;
        match next {
            Err(_) => panic!("connection still open 5s after oversized frame"),
            Ok(None) => break,         // stream ended — connection dropped
            Ok(Some(Err(_))) => break, // protocol/IO error — connection dead
            Ok(Some(Ok(Message::Close(_)))) => break, // clean close
            Ok(Some(Ok(Message::Text(s)))) => {
                panic!("oversized frame must not be dispatched, got echo: {s}")
            }
            Ok(Some(Ok(_))) => continue, // ping/pong noise — keep waiting
        }
    }
}
