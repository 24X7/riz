//! End-to-end WebSocket test. Spins up the server with a test-fixture chat
//! handler, connects with tokio-tungstenite, sends a message, and expects the
//! echo back via the @connections management API path.
//!
//! Requires `bun` on PATH (gated with `#[ignore]`).

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn websocket_echo_roundtrip() {
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
