//! Real-compute WASM example — proves the WASI runtime does actual application
//! logic, not just echo (Phase 5).
//!
//! Boots a real riz server with the `orders-wasm` module mounted at
//! `POST /orders`, then exercises the module's compute:
//!   * happy path — POSTs a valid order and asserts the structured pricing
//!     result: per-line extended amounts, subtotal, 8.25% tax, grand total,
//!     all in integer cents (deterministic across hosts).
//!   * validation path — POSTs an order with a non-positive qty and asserts the
//!     module rejects it with HTTP 422 and a structured error.
//!
//! The module is a `wasm32-wasip1` artifact built out-of-tree, so the test
//! SKIPS cleanly when it (or the riz host binary) isn't built — same convention
//! as the echo-wasm parity test.
//!
//! Run: `cargo nextest run --test wasm_examples`

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// ---------- artifact detection ----------

/// The compiled wasm32-wasip1 orders module (built by
/// `cargo build --release --target wasm32-wasip1` in examples/lambdas/orders-wasm).
fn orders_wasm_module() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/lambdas/orders-wasm/target/wasm32-wasip1/release/orders-wasm.wasm")
}

/// The built riz host binary — WasmRuntime re-invokes `riz __wasm-host`.
fn riz_host_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("release").join("riz")
}

fn orders_wasm_available() -> bool {
    let m = orders_wasm_module();
    let host = riz_host_binary();
    m.exists()
        && std::fs::metadata(&m)
            .map(|md| md.len() > 0)
            .unwrap_or(false)
        && host.exists()
}

// ---------- server boot (mirrors runtime_parity_request_shape.rs) ----------

async fn boot_riz(config_toml: &str) -> SocketAddr {
    let config: riz::config::Config = toml::from_str(config_toml).expect("toml parses");
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
        rate_limiter: tokio::sync::RwLock::new(riz::auth::api_key::RateLimiter::default()),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let bound = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.expect("axum::serve");
    });

    bound
}

async fn wait_for_ready(client: &reqwest::Client, url: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if client.get(url).send().await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server at {url} did not respond within 15s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const TIMEOUT_MS: i64 = 5000;

fn orders_config() -> (String, PathBuf) {
    let handler = orders_wasm_module();
    let config_toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.orders-wasm]
runtime = "wasm"
handler = "{handler}"
timeout_ms = {TIMEOUT_MS}
concurrency = 1

[[function.orders-wasm.routes]]
path = "/orders"
method = "POST"
"#,
        handler = handler.display()
    );
    (config_toml, handler)
}

/// Happy path — a valid order is priced correctly by the WASM module.
///
/// 2 × 500c + 3 × 250c = 1750c subtotal; tax = round(1750 × 825 / 10000) = 144c;
/// total = 1894c. All integer cents, deterministic across hosts.
#[tokio::test]
async fn wasm_orders_prices_a_valid_order() {
    if !orders_wasm_available() {
        eprintln!(
            "SKIP: orders-wasm module or riz host binary not built. Run \
             `cargo build --release` and \
             `cargo build --release --target wasm32-wasip1` in examples/lambdas/orders-wasm first."
        );
        return;
    }
    std::env::set_var("RIZ_HOST_BIN", riz_host_binary());
    let (config_toml, _) = orders_config();
    let addr = boot_riz(&config_toml).await;

    let client = reqwest::Client::new();
    wait_for_ready(&client, &format!("http://{addr}/orders")).await;

    let payload = serde_json::json!({
        "currency": "USD",
        "items": [
            { "sku": "WIDGET", "qty": 2, "unitPriceCents": 500 },
            { "sku": "GADGET", "qty": 3, "unitPriceCents": 250 }
        ]
    });
    let resp = client
        .post(format!("http://{addr}/orders"))
        .json(&payload)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200, "valid order should return 200");
    let body: serde_json::Value = resp.json().await.expect("json body");

    assert_eq!(body["currency"], "USD", "currency echoed; body = {body}");
    assert_eq!(body["lineItemCount"], 2, "two line items; body = {body}");
    assert_eq!(body["totalQuantity"], 5, "total qty = 2+3; body = {body}");
    assert_eq!(
        body["subtotalCents"], 1750,
        "subtotal = 1000+750; body = {body}"
    );
    assert_eq!(body["taxRateBps"], 825, "tax rate; body = {body}");
    assert_eq!(
        body["taxCents"], 144,
        "tax = round(1750*825/10000); body = {body}"
    );
    assert_eq!(
        body["totalCents"], 1894,
        "total = subtotal+tax; body = {body}"
    );

    // Per-line extended amounts prove the WASM did the arithmetic, not echo.
    assert_eq!(
        body["lines"][0]["sku"], "WIDGET",
        "line 0 sku; body = {body}"
    );
    assert_eq!(
        body["lines"][0]["extendedCents"], 1000,
        "2*500; body = {body}"
    );
    assert_eq!(
        body["lines"][1]["extendedCents"], 750,
        "3*250; body = {body}"
    );
}

/// Validation path — an order with a non-positive qty is rejected with 422 and a
/// structured error, proving the WASM module validates rather than echoes.
#[tokio::test]
async fn wasm_orders_rejects_invalid_quantity() {
    if !orders_wasm_available() {
        eprintln!("SKIP: orders-wasm module or riz host binary not built.");
        return;
    }
    std::env::set_var("RIZ_HOST_BIN", riz_host_binary());
    let (config_toml, _) = orders_config();
    let addr = boot_riz(&config_toml).await;

    let client = reqwest::Client::new();
    wait_for_ready(&client, &format!("http://{addr}/orders")).await;

    let payload = serde_json::json!({
        "currency": "USD",
        "items": [ { "sku": "BAD", "qty": 0, "unitPriceCents": 500 } ]
    });
    let resp = client
        .post(format!("http://{addr}/orders"))
        .json(&payload)
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        422,
        "qty <= 0 should be rejected as unprocessable"
    );
    let body: serde_json::Value = resp.json().await.expect("json body");
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("qty must be positive"),
        "error should explain the bad qty; got {body}"
    );
}

// ---------- riz-wasm ABI marker ----------

/// Every guest built on the riz-wasm shim exports the `riz_abi_v1` marker
/// symbol — the artifact-level half of the R1 conformance guarantee and the
/// host's future load-time wire handshake (spec 2026-07-19, PR10).
#[test]
fn shim_built_guests_export_the_abi_marker() {
    let modules = [
        orders_wasm_module(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parity/echo-wasm/target/wasm32-wasip1/release/echo-wasm.wasm"),
    ];
    let engine = wasmtime::Engine::default();
    let mut checked = 0;
    for path in modules {
        if !path.exists() {
            eprintln!("SKIP marker check: {} not built", path.display());
            continue;
        }
        let module = wasmtime::Module::from_file(&engine, &path)
            .unwrap_or_else(|e| panic!("{} should parse as wasm: {e}", path.display()));
        assert!(
            module.exports().any(|e| e.name() == "riz_abi_v1"),
            "{} lacks the riz_abi_v1 export — was it built on riz-wasm?",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no wasm module was built — nothing verified");
}
