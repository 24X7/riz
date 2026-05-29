//! Handler-source hot reload — Tier-2 #3.
//!
//! Existing tests/middleware_hotreload_e2e.rs covers riz.toml hot reload.
//! THIS test covers the day-to-day developer workflow: edit handler
//! source, save, and the next request hits the new code WITHOUT
//! touching riz.toml.
//!
//! Verification: capture the function's pool PID, modify the handler
//! source file, wait for the watcher's debounce + hot_swap, query
//! pool_stats again and assert the PID changed.
//!
//! Run: `cargo nextest run --test middleware_handler_source_hotreload`

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

/// Copy the echo-bun handler into a fresh temp dir so the test doesn't
/// disturb the example shared by other tests in the parity suite.
fn install_echo_handler(dir: &std::path::Path) -> PathBuf {
    let src = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/lambdas/echo-bun/index.ts"
    );
    let dst = dir.join("index.ts");
    std::fs::copy(src, &dst).expect("copy echo handler");
    dst
}

async fn boot_riz_with_handler(handler: &std::path::Path) -> (SocketAddr, Arc<riz::state::AppState>) {
    let handler_export = format!("{}.handler", handler.with_extension("").display());
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-bun]
runtime = "bun"
handler = "{handler_export}"
timeout_ms = 5000
concurrency = 1

[[function.echo-bun.routes]]
path = "/echo"
method = "GET"
"#
    );

    let config: riz::config::Config = toml::from_str(&config_toml).expect("toml");
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let metrics = riz::metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    let riz_state = Arc::new(riz::state::RizState::new());
    let stage = config.server.stage.clone();
    let default_ttl = config.cache.default_ttl_secs;
    for (name, cfg) in &config.functions {
        riz_state
            .register(riz::state::FunctionState::user(
                name.clone(),
                cfg.clone(),
                &stage,
                default_ttl,
            ))
            .await;
    }

    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    process_manager
        .spawn_all(&config.functions, &registry, log_tx.clone())
        .await
        .expect("spawn_all");

    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config
        .functions
        .iter()
        .map(|(name, cfg)| {
            Arc::new(riz::runtime::process::ProcessHandler::for_function(
                name,
                cfg,
                process_manager.clone(),
            )) as Arc<dyn riz::runtime::LambdaHandler>
        })
        .collect();
    let router = riz::router::Router::new(handlers);

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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let bound = listener.local_addr().expect("local_addr");

    let server_state = app_state.clone();
    tokio::spawn(async move {
        let app = riz::server::build_app(server_state)
            .into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.expect("axum::serve");
    });

    (bound, app_state)
}

async fn current_pid(state: &Arc<riz::state::AppState>) -> u32 {
    let stats = state.process_manager.pool_stats().await;
    let s = stats
        .iter()
        .find(|p| p.name == "echo-bun")
        .expect("pool exists");
    assert_eq!(s.pids.len(), 1, "concurrency=1 → exactly one PID");
    s.pids[0]
}

#[tokio::test]
async fn handler_source_change_respawns_pool() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let handler_path = install_echo_handler(tmp.path());

    let (addr, app_state) = boot_riz_with_handler(&handler_path).await;

    // Spawn the handler-source watcher on this AppState.
    let watcher_state = app_state.clone();
    tokio::spawn(async move {
        riz::hotreload::watch_handler_sources(watcher_state).await;
    });
    // Let the watcher subscribe before we touch any files.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Warm the pool via one request.
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/echo");
    let _ = client.get(&url).send().await.expect("warmup GET");
    let initial_pid = current_pid(&app_state).await;
    assert!(initial_pid > 0, "warmup must produce a real PID");

    // Modify the handler source: append a meaningless comment.
    // notify::EventKind::Modify fires on write + close.
    let mut contents = std::fs::read_to_string(&handler_path).expect("read handler");
    contents.push_str("\n// hot-reload trigger\n");
    std::fs::write(&handler_path, contents).expect("rewrite handler");

    // Wait up to 5s for the watcher's debounce + hot_swap to fire and
    // for the pool PID to change.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut observed_pid = initial_pid;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        observed_pid = current_pid(&app_state).await;
        if observed_pid != initial_pid {
            break;
        }
    }
    assert_ne!(
        observed_pid, initial_pid,
        "handler source change must trigger hot_swap — PID didn't change within 5s"
    );

    // Sanity: the new pool is still serviceable.
    let resp = client.get(&url).send().await.expect("post-reload GET");
    assert_eq!(
        resp.status(),
        200,
        "post-reload request must still return 200"
    );
}
