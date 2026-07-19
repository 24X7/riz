//! Python WebSocket handler end-to-end.
//!
//! The Bun WebSocket path is covered by tests/websocket_integration.rs.
//! THIS test exercises the same flow with the Python runtime adapter:
//!   1. Boot riz with a Python chat handler (function.chat, protocol = "websocket")
//!   2. WS-connect a client to /chat
//!   3. Send a text message
//!   4. Assert the handler's "echo: <msg>" reply arrives via the @connections POST
//!
//! Confirms `invoke_generic` works for any runtime adapter, not just Bun.
//!
//! Run: `cargo nextest run --test middleware_ws_python`

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

#[tokio::test]
async fn python_ws_echo_roundtrip() {
    if !python3_available() {
        eprintln!("SKIP: python3 not on PATH");
        return;
    }
    let handler_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/chat-python/main.lambda_handler"
    );

    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.chat]
protocol    = "websocket"
runtime     = "python"
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

    // The Python handler reads RIZ_TEST_BASE_URL — set it BEFORE spawn_all
    // so the python3 child processes inherit it.
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
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    });

    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    // Give python3 a moment to boot.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect must succeed");

    socket
        .send(Message::Text("hello from python".into()))
        .await
        .unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("no reply within 5s")
        .expect("stream ended")
        .expect("ws read error");

    match reply {
        Message::Text(s) => assert_eq!(
            s, "echo: hello from python",
            "python WS handler must echo via @connections POST"
        ),
        other => panic!("expected text frame, got {other:?}"),
    }

    socket.close(None).await.unwrap();
}
