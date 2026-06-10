//! Rust WebSocket handler end-to-end.
//!
//! Mirrors tests/middleware_ws_python.rs but with the Rust runtime
//! adapter and the chat-rust example binary. Proves the Rust adapter
//! path handles WS events with the same correctness as Bun + Python.
//!
//! Run: `cargo nextest run --test middleware_ws_rust`
//! Pre-req: `cargo build --release -p chat-rust`

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn chat_rust_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("release").join("chat-rust")
}

#[tokio::test]
async fn rust_ws_echo_roundtrip() {
    let bin = chat_rust_binary();
    if !bin.exists() {
        eprintln!(
            "SKIP: chat-rust binary not built at {}. \
             Run `cargo build --release -p chat-rust` first.",
            bin.display()
        );
        return;
    }

    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.chat]
protocol    = "websocket"
runtime     = "rust"
handler     = "{handler}"
timeout_ms  = 5000
concurrency = 4

[[function.chat.routes]]
path = "/chat"
"#,
        handler = bin.display()
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    config.validate().unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    // The Rust handler reads RIZ_TEST_BASE_URL — set it BEFORE spawn_all
    // so the chat-rust child processes inherit it.
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

    // Give the rust binary a moment to boot + initialize its tokio runtime.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect must succeed");

    socket
        .send(Message::Text("hello from rust".into()))
        .await
        .unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("no reply within 5s")
        .expect("stream ended")
        .expect("ws read error");

    match reply {
        Message::Text(s) => assert_eq!(
            s, "echo: hello from rust",
            "rust WS handler must echo via @connections POST"
        ),
        other => panic!("expected text frame, got {other:?}"),
    }

    socket.close(None).await.unwrap();
}
