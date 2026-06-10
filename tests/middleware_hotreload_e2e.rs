//! Hot-reload end-to-end via HTTP — Slice L.
//!
//! The existing `tests/hotreload_integration.rs` covers the watcher →
//! RizState wiring at the unit level (no HTTP listener, no real
//! request flow). This complementary test exercises the operator-level
//! experience:
//!
//!   1. Boot riz with an initial riz.toml that mounts /echo on a Bun handler
//!   2. GET /echo returns 200
//!   3. Rewrite riz.toml to ALSO mount /reloaded on the same handler
//!   4. Wait for the watcher to propagate (poll until /reloaded is reachable)
//!   5. GET /reloaded returns 200    (new route live, no restart needed)
//!   6. GET /echo still returns 200  (old route preserved across reload)
//!
//! Catches regressions where hot-reload updates RizState but doesn't
//! rebuild the live axum Router used by the active server.
//!
//! Run: `cargo nextest run --test middleware_hotreload_e2e`

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

const TIMEOUT_MS: i64 = 5000;
const ECHO_BUN_HANDLER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/lambdas/echo-bun/index.handler"
);

fn riz_toml_with_routes(routes: &[(&str, &str)]) -> String {
    let routes_block: String = routes
        .iter()
        .map(|(path, method)| {
            format!(
                "[[function.echo-bun.routes]]\npath = \"{path}\"\nmethod = \"{method}\"\n"
            )
        })
        .collect();
    format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.echo-bun]
runtime = "bun"
handler = "{ECHO_BUN_HANDLER}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

{routes_block}
"#
    )
}

async fn boot_riz_from_path(config_path: PathBuf) -> (SocketAddr, Arc<riz::state::AppState>) {
    let toml_str = std::fs::read_to_string(&config_path).expect("read");
    let config: riz::config::Config = toml::from_str(&toml_str).expect("toml parses");

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
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
        telemetry,
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

async fn poll_until_reachable(client: &reqwest::Client, url: &str, status: u16, deadline_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().as_u16() == status {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn hotreload_picks_up_new_route_end_to_end() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }

    let tmpdir = tempfile::TempDir::new().expect("tempdir");
    let config_path = tmpdir.path().join("riz.toml");
    std::fs::write(&config_path, riz_toml_with_routes(&[("/echo", "GET")]))
        .expect("write initial config");

    let (addr, app_state) = boot_riz_from_path(config_path.clone()).await;

    // Spawn the hot-reload watcher on the same AppState.
    let watcher_state = app_state.clone();
    let watcher_path = config_path.display().to_string();
    tokio::spawn(async move {
        riz::hotreload::watch_config(watcher_path, watcher_state).await;
    });
    // Let the watcher subscribe to the file before we touch it.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = reqwest::Client::new();
    let echo_url = format!("http://{addr}/echo");
    let reloaded_url = format!("http://{addr}/reloaded");

    // Sanity: initial route is live.
    assert!(
        poll_until_reachable(&client, &echo_url, 200, 10).await,
        "initial GET /echo must return 200"
    );

    // /reloaded should NOT be routable yet.
    let pre_reload = client.get(&reloaded_url).send().await.expect("send");
    assert_ne!(
        pre_reload.status().as_u16(),
        200,
        "/reloaded must NOT be live before hot-reload"
    );

    // Rewrite the config to add /reloaded as a second route on the same fn.
    std::fs::write(
        &config_path,
        riz_toml_with_routes(&[("/echo", "GET"), ("/reloaded", "GET")]),
    )
    .expect("rewrite config");

    // Wait for the watcher debounce + propagation. /reloaded should
    // become reachable within ~5s.
    assert!(
        poll_until_reachable(&client, &reloaded_url, 200, 10).await,
        "after rewrite, GET /reloaded must return 200 within 10s (hot-reload didn't propagate)"
    );

    // The original route must still work — hot-reload should be additive,
    // not destructive.
    let after = client.get(&echo_url).send().await.expect("send");
    assert_eq!(
        after.status().as_u16(),
        200,
        "GET /echo must still return 200 after hot-reload (regression: old routes lost)"
    );
}
