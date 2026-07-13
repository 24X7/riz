//! 8.3 — hot_swap-under-load race tests.
//!
//! Gate: requires `bun` on PATH. Skips the test (via `return`) if absent.
//!
//! Spin up a pool with the bun echo handler at concurrency=4, fire 200
//! concurrent invocations via `ProcessManager::invoke`, trigger `hot_swap`
//! halfway through (after 100 are in flight), wait for all 200 to complete,
//! then assert zero 5xx responses and no 502s from killed handles.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use riz::config::{Config, FunctionConfig, RouteSpec, RuntimeKind};

/// Helper — identical to the one in `integration_test.rs` and `http_boundary.rs`.
async fn boot_server(
    config: Config,
    registry: Arc<riz::process::runtime::RuntimeRegistry>,
) -> (Arc<riz::state::AppState>, SocketAddr) {
    use riz::state::{FunctionState, RizState};

    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(64_000);
    let riz_state = Arc::new(RizState::new());
    let stage = config.server.stage.clone();
    let default_ttl = config.cache.default_ttl_secs;

    for (name, cfg) in &config.functions {
        riz_state
            .register(FunctionState::user(
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
        .expect("spawn_all must succeed");

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
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let state_clone = state.clone();
    tokio::spawn(async move {
        let app =
            riz::server::build_app(state_clone).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    (state, addr)
}

/// Check if `bun` is available on PATH.
fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

/// Build a `FunctionConfig` for the echo handler at `concurrency`.
fn echo_fn(concurrency: usize) -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/echo-lambda/index.ts"
        )),
        timeout_ms: 5000,
        integration_timeout_ms: 30000,
        stage_variables: Default::default(),
        env: Default::default(),
        cache_ttl_secs: None,
        concurrency,
        routes: vec![RouteSpec {
            path: "/echo".into(),
            method: "GET".into(),
        }],
        cors: None,
        authorizer: None,
        memory_mb: None,
        cpu_time_secs: None,
        allowed_paths: None,
        mcp: None,
        capabilities: Default::default(),
        guard_in: None,
        guard_out: None,
    }
}

/// Fire 200 concurrent GET /echo requests against the running server and
/// return a list of HTTP status codes.  Requests are fired in two batches of
/// 100 so the hot_swap can be triggered between them.
async fn fire_requests(addr: SocketAddr, n: usize) -> Vec<u16> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let url = format!("http://{addr}/echo");

    let futs: Vec<_> = (0..n)
        .map(|_| {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                match client.get(&url).send().await {
                    Ok(r) => r.status().as_u16(),
                    Err(_) => 502,
                }
            })
        })
        .collect();

    let mut statuses = Vec::with_capacity(n);
    for fut in futs {
        statuses.push(fut.await.unwrap_or(502));
    }
    statuses
}

/// Main hot_swap-under-load race test.
///
/// 1. Boot the server with concurrency=4 echo handler.
/// 2. Fire 100 concurrent requests (first batch).
/// 3. While those are settling, trigger `hot_swap` with a new FunctionConfig
///    that has a longer `timeout_ms`.
/// 4. Fire another 100 concurrent requests (second batch).
/// 5. Collect all results — assert zero 5xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hot_swap_under_load_no_5xx() {
    if !bun_available() {
        eprintln!("hot_swap_under_load_no_5xx: bun not on PATH — skipping");
        return;
    }

    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));

    let mut functions = IndexMap::new();
    functions.insert("echo".to_string(), echo_fn(4));

    let config = Config {
        functions,
        ..Default::default()
    };

    let (state, addr) = boot_server(config, registry.clone()).await;

    // Wait for the server to be ready.
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if client
            .get(format!("http://{addr}/echo"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server did not start within 15s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Batch 1: fire 100 concurrent requests.
    let batch1 = fire_requests(addr, 100);

    // Trigger hot_swap concurrently with batch 1 running.
    let state_swap = state.clone();
    let swap_task = tokio::spawn(async move {
        // Small delay so some batch-1 requests are in flight.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let new_cfg = echo_fn(4); // same handler, new config object
        let result = state_swap.process_manager.hot_swap("echo", new_cfg).await;
        result.expect("hot_swap must succeed");
    });

    // Wait for batch 1 and the swap to complete.
    let (statuses1, _) = tokio::join!(batch1, swap_task);

    // Batch 2: fire another 100 concurrent requests after swap.
    let statuses2 = fire_requests(addr, 100).await;

    // Collect all 200 statuses and assert no 5xx.
    let all_statuses: Vec<u16> = statuses1.into_iter().chain(statuses2).collect();
    let fives: Vec<u16> = all_statuses.iter().copied().filter(|&s| s >= 500).collect();

    assert!(
        fives.is_empty(),
        "hot_swap under load must produce zero 5xx; got {} 5xx out of 200 total: {:?}",
        fives.len(),
        &fives[..fives.len().min(10)]
    );
}
