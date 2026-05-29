//! WebSocket load test + BUG-15 verification.
//!
//! BUG-15 (route_stats write lock serializes all requests) was the
//! shape of the bug as filed pre-wave-7.3. Wave-7.3 ("kill the dual
//! stats system") removed the global write-lock path entirely — see
//! `src/state.rs::record_invocation` (lines 477-508): now uses a
//! RwLock READ on `functions` (concurrent ok) + atomic fetch_add for
//! the counters + per-entry std::sync::Mutex on `latency` and
//! `last_invoked` (each held for a single assignment / reservoir push).
//!
//! This test exercises the WS hot path under sustained throughput and
//! asserts no pathological queueing. If a hidden global lock existed,
//! the 100 messages would serialize and time out well past the
//! 10-second ceiling.
//!
//! Pre-req: bun on PATH (the chat fixture is Bun).
//! Run: `cargo nextest run --test perf_ws_load`

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

const N_MESSAGES: usize = 100;

#[tokio::test]
async fn ws_handles_100_messages_within_10s() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }

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
concurrency = 8

[[function.chat.routes]]
path = "/chat"
"#,
        handler = handler_path
    );

    let config: riz::config::Config = toml::from_str(&config_toml).unwrap();
    config.validate().unwrap();

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(100_000);

    let riz_state = Arc::new(riz::state::RizState::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
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
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let url = format!("ws://{addr}/chat");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");

    // Fire N messages without waiting for individual replies — let the
    // tokio runtime + the bun pool fan out across the 8 concurrency
    // slots. Capture wall-clock start to assert total throughput.
    let started = Instant::now();
    for i in 0..N_MESSAGES {
        socket
            .send(Message::Text(format!("msg-{i}").into()))
            .await
            .expect("ws send");
    }

    // Drain replies. Each handler invocation POSTs back via
    // @connections, so we should observe N text frames in any order.
    let mut received = 0usize;
    while received < N_MESSAGES {
        let next = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "BUG-15 contention check: only {received}/{N_MESSAGES} replies within 10s — \
                     pathological serialization in the WS hot path"
                )
            })
            .expect("stream ended")
            .expect("ws read");
        if matches!(next, Message::Text(_)) {
            received += 1;
        }
    }
    let elapsed = started.elapsed();

    assert_eq!(received, N_MESSAGES);
    // Sanity check: a fully-serialized path would take seconds-per-message
    // (handler does a real HTTP POST round-trip back via @connections).
    // 10s for 100 messages = 100ms per message average, which leaves
    // plenty of headroom for the genuine work.
    assert!(
        elapsed < Duration::from_secs(10),
        "100 WS messages must complete within 10s (got {elapsed:?}); \
         BUG-15 may have regressed"
    );

    eprintln!("ws-load: {N_MESSAGES} round-trips in {elapsed:?}");
    socket.close(None).await.unwrap();
}
