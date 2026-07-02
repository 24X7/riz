//! MCP progress notifications during tool calls (v1 roadmap #12).
//!
//! A tools/call carrying `params._meta.progressToken` over the SSE transport
//! receives `notifications/progress` frames while the handler runs — token
//! echoed verbatim, `progress` strictly increasing — followed by the final
//! JSON-RPC response frame, after which the stream ends. Calls without a
//! token get exactly one message frame (no invented notifications).

use std::net::SocketAddr;
use std::sync::Arc;

use indexmap::IndexMap;
use riz::config::{Config, FunctionConfig, RuntimeKind};

fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok()
}

fn slow_fn_cfg() -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Bun,
        protocol: Default::default(),
        handler: std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sleep-lambda/index.ts"
        )),
        timeout_ms: 60_000,
        integration_timeout_ms: 30_000,
        stage_variables: Default::default(),
        env: Default::default(),
        cache_ttl_secs: None,
        concurrency: 1,
        routes: vec![riz::config::RouteSpec {
            path: "/slow".into(),
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

/// State with the MCP handler + a real bun-backed slow function ("slow-fn",
/// sleeps 3000ms) wired through the same Router the MCP tool call dispatches
/// through reentrantly.
async fn make_state_with_slow_fn() -> Arc<riz::state::AppState> {
    let mut functions = IndexMap::new();
    functions.insert("slow-fn".to_string(), slow_fn_cfg());
    let config = Config {
        functions: functions.clone(),
        ..Default::default()
    };
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);

    riz_state
        .register(riz::state::FunctionState::system(
            "_riz_mcp",
            vec!["POST /_riz/mcp".into()],
            "$default",
        ))
        .await;
    riz_state
        .register(riz::state::FunctionState::user(
            "slow-fn",
            slow_fn_cfg(),
            "$default",
            0,
        ))
        .await;

    let mcp = Arc::new(riz::system::mcp::McpHandler::new(riz_state.clone(), None));
    let mut handlers: Vec<Arc<dyn riz::runtime::LambdaHandler>> = config
        .functions
        .iter()
        .map(|(name, cfg)| {
            let h = riz::runtime::process::ProcessHandler::for_function(
                name,
                cfg,
                process_manager.clone(),
            );
            Arc::new(h) as Arc<dyn riz::runtime::LambdaHandler>
        })
        .collect();
    handlers.push(mcp.clone() as Arc<dyn riz::runtime::LambdaHandler>);
    let router_arc = Arc::new(riz::router::Router::new(handlers.clone()));
    mcp.set_router(router_arc).await;

    let state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(handlers)),
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

    // Spawn the real bun pool for slow-fn.
    let registry = state.runtime_registry.clone();
    state
        .process_manager
        .spawn_all(&functions, &registry, state.log_tx.clone())
        .await
        .expect("spawn_all must succeed");
    state
}

async fn serve(state: Arc<riz::state::AppState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Collect every SSE `data:` payload from a response until the stream ends.
async fn collect_sse_payloads(resp: reqwest::Response) -> Vec<serde_json::Value> {
    use futures_util::StreamExt;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(20), stream.next())
        .await
        .expect("SSE frame within 20s")
    {
        buf.push_str(&String::from_utf8_lossy(&chunk.expect("chunk reads")));
    }
    buf.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).expect("SSE data is JSON"))
        .collect()
}

fn call_with_token(token: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc":"2.0","id":9,"method":"tools/call",
        "params":{
            "name":"slow-fn",
            "arguments":{},
            "_meta":{"progressToken": token}
        }
    })
}

#[tokio::test]
async fn progress_notifications_stream_during_slow_tool_call() {
    if !bun_available() {
        eprintln!("mcp_progress: bun not on PATH — skipping");
        return;
    }
    let addr = serve(make_state_with_slow_fn().await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .json(&call_with_token(serde_json::json!("tok-1")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let payloads = collect_sse_payloads(resp).await;

    let (progress, rest): (Vec<_>, Vec<_>) = payloads
        .iter()
        .partition(|p| p["method"] == "notifications/progress");
    // 3000ms handler at a 500ms tick → expect several; require ≥2 to stay
    // robust under CI scheduling jitter.
    assert!(
        progress.len() >= 2,
        "expected >=2 progress frames, got {payloads:?}"
    );
    // Token echoed verbatim, progress strictly increasing.
    let mut last = 0.0f64;
    for p in &progress {
        assert_eq!(p["params"]["progressToken"], "tok-1", "{p}");
        let v = p["params"]["progress"].as_f64().unwrap();
        assert!(v > last, "progress must increase: {payloads:?}");
        last = v;
    }
    // Exactly one final response frame, carrying the JSON-RPC result, LAST.
    assert_eq!(rest.len(), 1, "{payloads:?}");
    let final_frame = rest[0];
    assert_eq!(final_frame["id"], 9, "{final_frame}");
    assert!(final_frame["result"]["content"].is_array(), "{final_frame}");
    assert_eq!(
        payloads.last().unwrap()["id"],
        9,
        "response must be the last frame: {payloads:?}"
    );
}

#[tokio::test]
async fn numeric_progress_token_is_echoed_as_a_number() {
    if !bun_available() {
        eprintln!("mcp_progress: bun not on PATH — skipping");
        return;
    }
    let addr = serve(make_state_with_slow_fn().await).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .json(&call_with_token(serde_json::json!(42)))
        .send()
        .await
        .unwrap();
    let payloads = collect_sse_payloads(resp).await;
    let progress: Vec<_> = payloads
        .iter()
        .filter(|p| p["method"] == "notifications/progress")
        .collect();
    assert!(!progress.is_empty(), "{payloads:?}");
    // Spec: progressToken is string | number — 42 must stay a number.
    assert_eq!(progress[0]["params"]["progressToken"], 42, "{payloads:?}");
}

#[tokio::test]
async fn no_progress_frames_without_a_token() {
    if !bun_available() {
        eprintln!("mcp_progress: bun not on PATH — skipping");
        return;
    }
    let addr = serve(make_state_with_slow_fn().await).await;
    let call = serde_json::json!({
        "jsonrpc":"2.0","id":9,"method":"tools/call",
        "params":{"name":"slow-fn","arguments":{}}
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/_riz/mcp"))
        .header("accept", "text/event-stream")
        .json(&call)
        .send()
        .await
        .unwrap();
    let payloads = collect_sse_payloads(resp).await;
    assert_eq!(payloads.len(), 1, "no token → no progress: {payloads:?}");
    assert_eq!(payloads[0]["id"], 9);
}
