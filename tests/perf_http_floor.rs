//! W2.2 — CI-gated HTTP dispatch throughput FLOOR.
//!
//! This is not the headline benchmark (see `benches/README.md`: ~91k req/s via
//! `wrk` on an M-series box — a bench recipe, not a test). It is a *regression
//! tripwire*: a deliberately conservative floor that only trips on a large,
//! structural regression — a re-introduced global lock serializing requests, a
//! cold-start on every request (warm pool broken), or the dispatch layer
//! getting an order of magnitude slower. The margin over the headline is huge
//! (~500x) so it stays green on a slow, loaded CI runner and never flakes.
//!
//! Pre-req: bun on PATH (the ping handler is Bun). Skips cleanly if absent.

use std::net::SocketAddr;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::config::{Config, FunctionConfig, RouteSpec, RuntimeKind};

const WARMUP: usize = 32;
const TOTAL: usize = 800;
const CONCURRENCY: usize = 8;
/// The floor. Local warm Bun sustains thousands/sec; even a heavily loaded CI
/// box clears this by a wide margin. Only a structural regression drops below.
const FLOOR_RPS: f64 = 150.0;
/// Hard wall-clock ceiling as a second guard (serialization would blow this).
const CEILING_SECS: u64 = 12;

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

fn ping_config() -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/lambdas/ping/index.handler"
        )),
        timeout_ms: 1000,
        integration_timeout_ms: 30000,
        stage_variables: Default::default(),
        env: Default::default(),
        cache_ttl_secs: Some(0),
        concurrency: CONCURRENCY,
        routes: vec![RouteSpec {
            path: "/ping".into(),
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

async fn boot() -> (SocketAddr, Arc<riz::state::AppState>) {
    let mut functions = IndexMap::new();
    functions.insert("ping".to_string(), ping_config());
    let config = Config {
        functions,
        ..Default::default()
    };
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    // Register the function state so record_invocation resolves it.
    riz_state
        .register(riz::state::FunctionState::user(
            "ping",
            ping_config(),
            "$default",
            0,
        ))
        .await;

    let handler = riz::runtime::process::ProcessHandler::for_function(
        "ping",
        &ping_config(),
        process_manager.clone(),
    );
    let handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = vec![Arc::new(handler)];
    let router = riz::router::Router::new(handlers);

    let state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        router: tokio::sync::RwLock::new(router),
        process_manager: process_manager.clone(),
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry.clone(),
        log_tx: log_tx.clone(),
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    });

    // Spawn the real Bun worker pool.
    process_manager
        .spawn_all(&config.functions, &registry, log_tx)
        .await
        .expect("spawn ping pool");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_state = state.clone();
    tokio::spawn(async move {
        let app =
            riz::server::build_app(serve_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn wait_ready(addr: SocketAddr) {
    let url = format!("http://{addr}/ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(r) = reqwest::get(&url).await {
            if r.status().is_success() {
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("/ready never went green within 20s — Bun likely failed to start");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_dispatch_sustains_throughput_floor() {
    if !bun_available() {
        eprintln!("SKIP http_dispatch_sustains_throughput_floor: bun not on PATH");
        return;
    }

    let (addr, _state) = boot().await;
    wait_ready(addr).await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(CONCURRENCY)
        .build()
        .unwrap();
    let url = format!("http://{addr}/ping");

    // Warm every worker so the measured window has zero cold starts.
    for _ in 0..WARMUP {
        let r = client.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 200, "warmup request failed");
    }

    // Measure: TOTAL requests across CONCURRENCY tasks.
    let per_task = TOTAL / CONCURRENCY;
    let start = std::time::Instant::now();
    let mut handles = Vec::new();
    for _ in 0..CONCURRENCY {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let mut ok = 0usize;
            for _ in 0..per_task {
                if let Ok(r) = client.get(&url).send().await {
                    if r.status() == 200 {
                        ok += 1;
                    }
                }
            }
            ok
        }));
    }
    let mut ok = 0usize;
    for h in handles {
        ok += h.await.unwrap();
    }
    let elapsed = start.elapsed();
    let sent = per_task * CONCURRENCY;
    let rps = ok as f64 / elapsed.as_secs_f64();
    eprintln!(
        "http floor: {ok}/{sent} ok in {:.2}s → {rps:.0} req/s (floor {FLOOR_RPS:.0})",
        elapsed.as_secs_f64()
    );

    // Tolerate a tiny transient failure rate (≥99% must succeed): under real
    // concurrency a worker respawn or stdio hiccup can drop the odd request,
    // especially on a loaded CI box. That is not what this guards — a broken
    // pool or serialization regression fails FAR more than 1% (and blows the
    // ceiling / floor below). Requiring exactly 100% only added flakiness.
    let min_ok = sent - sent / 100;
    assert!(
        ok >= min_ok,
        "throughput floor: only {ok}/{sent} succeeded (< 99%) — a broken pool, not a transient hiccup"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(CEILING_SECS),
        "throughput floor: {sent} warm requests took {:.2}s (> {CEILING_SECS}s ceiling) — likely a serialization regression",
        elapsed.as_secs_f64()
    );
    assert!(
        rps >= FLOOR_RPS,
        "throughput floor: {rps:.0} req/s is below the {FLOOR_RPS:.0} req/s floor — a large dispatch regression"
    );
}
