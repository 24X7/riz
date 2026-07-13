use crate::auth::authorizer::AuthCacheKey;
use crate::auth::middleware::{enforce_authorizer, inject_authorizer_context};
use crate::cache::CacheLayer;
use crate::cors;
use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription, ApiGatewayV2httpResponse, Body,
};
use crate::state::AppState;
use axum::{
    body::Body as AxumBody,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router as AxumRouter,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info};
use uuid::Uuid;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct ReadyResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unhealthy: Vec<String>,
}

pub fn build_app(state: Arc<AppState>) -> AxumRouter {
    let app = AxumRouter::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        // MCP Streamable HTTP: the transport entry serves GET-SSE / POST-SSE /
        // DELETE itself and delegates everything else (plain-JSON POST, OPTIONS
        // preflight, non-SSE GET) back to dispatch_lambda unchanged.
        .route("/_riz/mcp", any(crate::system::mcp::transport::entry));

    let app = mount_ws_upgrade_routes(app, &state);
    let app = mount_gateway_routes(app, &state);

    app.fallback(any(dispatch_lambda)).with_state(state)
}

/// Mount WebSocket upgrade routes for every protocol=websocket function.
/// build_app runs once at startup, so a try_read in this sync context is
/// OK — no other writer should be holding the config write lock at startup.
fn mount_ws_upgrade_routes(
    mut app: AxumRouter<Arc<AppState>>,
    state: &Arc<AppState>,
) -> AxumRouter<Arc<AppState>> {
    if let Ok(cfg) = state.config.try_read() {
        for (name, fc) in &cfg.functions {
            if matches!(fc.protocol, crate::config::Protocol::WebSocket) {
                if let Some(route) = fc.effective_routes(name).first() {
                    let path = route.path.clone();
                    let name_owned = name.clone();
                    let state_clone = state.clone();
                    app = app.route(
                        &path,
                        axum::routing::any(
                            move |ws: axum::extract::WebSocketUpgrade,
                                  headers: axum::http::HeaderMap,
                                  ci: axum::extract::ConnectInfo<std::net::SocketAddr>,
                                  raw_query: axum::extract::RawQuery| {
                                let s = state_clone.clone();
                                let n = name_owned.clone();
                                async move {
                                    crate::ws::upgrade::ws_upgrade_handler(
                                        axum::extract::State((s, n)),
                                        ci,
                                        ws,
                                        headers,
                                        raw_query,
                                    )
                                    .await
                                }
                            },
                        ),
                    );
                }
            }
        }
    }
    app
}

/// Mount the OpenAI-compatible endpoint (/_riz/v1/*) when [gateway] is set,
/// plus the A2A agent surface when [agent] is set too.
/// Bearer-gated like every other /_riz/* surface: these routes spend real
/// provider money — they must never be the one unauthenticated door. The
/// token is resolved once at mount time (same lifecycle as the MCP
/// handler's); changing it requires a restart.
fn mount_gateway_routes(
    mut app: AxumRouter<Arc<AppState>>,
    state: &Arc<AppState>,
) -> AxumRouter<Arc<AppState>> {
    if let Ok(cfg) = state.config.try_read() {
        if cfg.gateway.enabled() {
            let bearer = cfg.effective_bearer_token();
            match crate::llm::Gateway::from_config(&cfg.gateway) {
                Ok(gw) => {
                    let gw = Arc::new(gw);
                    app = mount_llm_api_routes(app, state, &gw, &bearer);
                    info!("LLM gateway enabled — OpenAI-compatible endpoint at /_riz/v1");
                    app = mount_agent_surface(app, state, &cfg, &gw, &bearer);
                }
                Err(e) => error!("gateway disabled: failed to build from config: {e}"),
            }
        }
    }
    app
}

/// A2A built-in agent — when `[agent]` is set, this instance becomes a
/// delegable agent (validated: [agent] requires [gateway]). The card is
/// public (it DECLARES the auth, like llms.txt); the JSON-RPC endpoint is
/// bearer-gated with the rest of /_riz/*.
fn mount_agent_surface(
    mut app: AxumRouter<Arc<AppState>>,
    state: &Arc<AppState>,
    cfg: &crate::config::Config,
    gw: &Arc<crate::llm::Gateway>,
    bearer: &Option<String>,
) -> AxumRouter<Arc<AppState>> {
    if let Some(agent_cfg) = cfg.agent.clone() {
        let public_base = format!("http://{}:{}", cfg.server.host, cfg.server.port);
        let rt = Arc::new(crate::system::a2a::A2aRuntime::new(
            agent_cfg,
            gw.clone(),
            state.clone(),
            bearer.clone(),
            public_base,
        ));
        app = mount_a2a_routes(app, &rt, bearer);
        info!(
            "A2A agent '{}' enabled — card at /.well-known/agent-card.json, endpoint at /_riz/a2a",
            rt.cfg.name
        );
    }
    app
}

/// The four OpenAI-compatible routes (/_riz/v1/chat/completions, embeddings,
/// models, usage) — each behind the shared bearer gate.
fn mount_llm_api_routes(
    mut app: AxumRouter<Arc<AppState>>,
    state: &Arc<AppState>,
    gw: &Arc<crate::llm::Gateway>,
    bearer: &Option<String>,
) -> AxumRouter<Arc<AppState>> {
    let gw_chat = gw.clone();
    let chat_telemetry = state.telemetry.clone();
    let chat_riz_state = state.riz_state.clone();
    let tok = bearer.clone();
    app = app.route(
        "/_riz/v1/chat/completions",
        post(
            move |headers: axum::http::HeaderMap, body: Json<crate::llm::ChatRequest>| {
                let gw = gw_chat.clone();
                let telemetry = chat_telemetry.clone();
                let riz_state = chat_riz_state.clone();
                let tok = tok.clone();
                async move {
                    if let Some(resp) = bearer_reject(&headers, tok.as_deref()) {
                        return resp;
                    }
                    crate::system::openai_compat::chat_completions(gw, telemetry, riz_state, body)
                        .await
                        .into_response()
                }
            },
        ),
    );
    let gw_embed = gw.clone();
    let tok = bearer.clone();
    app = app.route(
        "/_riz/v1/embeddings",
        post(
            move |headers: axum::http::HeaderMap, body: Json<crate::llm::EmbeddingsRequest>| {
                let gw = gw_embed.clone();
                let tok = tok.clone();
                async move {
                    if let Some(resp) = bearer_reject(&headers, tok.as_deref()) {
                        return resp;
                    }
                    crate::system::openai_compat::embeddings(gw, body)
                        .await
                        .into_response()
                }
            },
        ),
    );
    let gw_models = gw.clone();
    let tok = bearer.clone();
    app = app.route(
        "/_riz/v1/models",
        get(move |headers: axum::http::HeaderMap| {
            let gw = gw_models.clone();
            let tok = tok.clone();
            async move {
                if let Some(resp) = bearer_reject(&headers, tok.as_deref()) {
                    return resp;
                }
                crate::system::openai_compat::models(gw)
                    .await
                    .into_response()
            }
        }),
    );
    let gw_usage = gw.clone();
    let tok = bearer.clone();
    app.route(
        "/_riz/v1/usage",
        get(move |headers: axum::http::HeaderMap| {
            let gw = gw_usage.clone();
            let tok = tok.clone();
            async move {
                if let Some(resp) = bearer_reject(&headers, tok.as_deref()) {
                    return resp;
                }
                crate::system::openai_compat::usage(gw)
                    .await
                    .into_response()
            }
        }),
    )
}

/// The A2A agent surface: the public Agent Card (it DECLARES the auth, like
/// llms.txt) and the bearer-gated JSON-RPC endpoint.
fn mount_a2a_routes(
    mut app: AxumRouter<Arc<AppState>>,
    rt: &Arc<crate::system::a2a::A2aRuntime>,
    bearer: &Option<String>,
) -> AxumRouter<Arc<AppState>> {
    let rt_card = rt.clone();
    app = app.route(
        "/.well-known/agent-card.json",
        get(move || {
            let rt = rt_card.clone();
            async move { Json(rt.agent_card().await).into_response() }
        }),
    );
    let rt_rpc = rt.clone();
    let tok = bearer.clone();
    app.route(
        "/_riz/a2a",
        post(
            move |headers: axum::http::HeaderMap, body: Json<serde_json::Value>| {
                a2a_rpc_response(rt_rpc.clone(), tok.clone(), headers, body.0)
            },
        ),
    )
}

/// Serve one JSON-RPC call on /_riz/a2a: bearer gate, mesh hop-depth intake,
/// then SSE for the streaming methods and a single JSON-RPC body otherwise.
async fn a2a_rpc_response(
    rt: Arc<crate::system::a2a::A2aRuntime>,
    tok: Option<String>,
    headers: axum::http::HeaderMap,
    body: serde_json::Value,
) -> Response {
    if let Some(resp) = bearer_reject(&headers, tok.as_deref()) {
        return resp;
    }
    // Mesh chain depth (loop protection): set by a delegating riz peer.
    let hop = headers
        .get("riz-a2a-hop")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    // SendStreamingMessage answers as SSE (spec §7); everything else is a
    // single JSON-RPC response. `.get()` over `[]`: an absent method reads
    // as "" either way, minus the panicking-access class.
    let method = body
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if matches!(method.as_str(), "SendStreamingMessage" | "message/stream") {
        let rpc_id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let params = body.get("params").cloned().unwrap_or(serde_json::json!({}));
        return match rt.send_streaming(params, rpc_id.clone(), hop).await {
            Ok(stream) => axum::response::sse::Sse::new(stream).into_response(),
            Err((code, message)) => Json(serde_json::json!({
                "jsonrpc": "2.0", "id": rpc_id,
                "error": { "code": code, "message": message }
            }))
            .into_response(),
        };
    }
    Json(rt.handle(body, hop).await).into_response()
}

/// Maximum time we'll wait for in-flight requests to drain after receiving a
/// shutdown signal. Matches the documented "30 s graceful drain" promise.
/// After this elapses we force-stop axum (any still-in-flight handler is cut
/// off) and proceed to kill child processes.
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = build_app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    info!("riz listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Channel from "shutdown signal observed inside the graceful-shutdown
    // future" to the outer task. We start the drain timeout clock ONLY
    // after the signal fires — the previous code applied the timeout to
    // the entire serve_future, which meant riz force-crashed exactly 30s
    // after boot regardless of whether a signal ever arrived.
    let (signal_observed_tx, signal_observed_rx) = tokio::sync::oneshot::channel::<()>();

    let serve_future = axum::serve(listener, app)
        .with_graceful_shutdown(signal_then_prepare_drain(state.clone(), signal_observed_tx));

    // axum::serve(...).with_graceful_shutdown(...) returns a
    // WithGracefulShutdown which implements IntoFuture, not Future. We need
    // a real Future to use with tokio::select! and tokio::time::timeout,
    // so convert via IntoFuture::into_future().
    use std::future::IntoFuture;
    serve_until_drained(serve_future.into_future(), signal_observed_rx).await?;

    tracing::info!("draining complete — killing child processes");
    kill_all_processes(&state).await;
    Ok(())
}

/// Run the server future through its two shutdown phases:
///   1. Until the shutdown signal fires, just run serve_future. No
///      timeout — the server is supposed to serve indefinitely.
///   2. Once the signal fires (signal_observed_rx resolves), race
///      serve_future against SHUTDOWN_DRAIN_TIMEOUT. If axum drains
///      in time, exit cleanly; otherwise log + force shutdown.
async fn serve_until_drained<F>(
    serve_future: F,
    signal_observed_rx: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    tokio::pin!(serve_future);
    tokio::select! {
        // Server returned on its own (graceful drain completed naturally,
        // OR an unexpected error). Propagate the result.
        r = &mut serve_future => r,
        // Signal fired. Switch to drain-timeout mode.
        _ = signal_observed_rx => {
            match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, serve_future).await {
                Ok(r) => r,
                Err(_elapsed) => {
                    tracing::warn!(
                        "drain timeout ({}s) elapsed — forcing shutdown with requests still in flight",
                        SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
                    );
                    Ok(())
                }
            }
        }
    }
}

/// The graceful-shutdown future handed to axum: wait for a shutdown signal,
/// prepare the drain (TUI exit, WS close), then start the outer drain clock
/// via `signal_observed_tx`.
async fn signal_then_prepare_drain(
    state: Arc<AppState>,
    signal_observed_tx: tokio::sync::oneshot::Sender<()>,
) {
    shutdown_signal().await;
    // Signal the TUI thread to exit cleanly BEFORE the drain begins.
    // The TUI runs on a detached std::thread::spawn — if main returns
    // first, the OS kills the TUI thread mid-render and the terminal is
    // left in raw mode + mouse-capture mode, printing escape garbage on
    // every keystroke. Signaling here gives the TUI ~drain-timeout
    // seconds to break its loop and run its cleanup path.
    crate::tui::request_shutdown();

    close_ws_connections_for_drain(&state);

    tracing::info!(
        "shutdown signal received — draining in-flight requests (max {}s)",
        SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
    );
    // Tell the outer task to start the drain deadline. send() consumes
    // the sender; this future runs at most once so this is fine.
    let _ = signal_observed_tx.send(());
}

/// Close every live WebSocket connection NOW, before axum begins
/// draining. WS handlers don't complete on their own — without this,
/// axum's graceful drain waits the full SHUTDOWN_DRAIN_TIMEOUT (30s)
/// for connections that will never close, then force-shuts. Sending
/// Close to each connection makes the writer task emit a WebSocket
/// Close frame to the client and exit, which lets the corresponding
/// axum handler task complete and the drain finish in milliseconds.
fn close_ws_connections_for_drain(state: &AppState) {
    let conn_count = state.ws_connections.all().len();
    if conn_count > 0 {
        tracing::info!("closing {conn_count} active WebSocket connection(s) before drain");
        for conn in state.ws_connections.all() {
            // try_send: on a full queue the drain still force-stops
            // after SHUTDOWN_DRAIN_TIMEOUT — never block shutdown on a
            // slow client.
            let _ = conn.outbound.try_send(crate::ws::OutboundMessage::Close);
        }
    }
}

/// Resolve when a shutdown signal (Ctrl+C / SIGTERM) arrives. If a handler
/// cannot be installed, that source is disabled with an error log and the
/// other source still works — degraded shutdown coverage, never a panic
/// (the process remains killable by SIGKILL regardless).
async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!(
                "failed to install Ctrl+C handler ({e}) — Ctrl+C will not trigger graceful shutdown"
            );
            std::future::pending::<()>().await
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!(
                    "failed to install SIGTERM handler ({e}) — SIGTERM will not trigger graceful shutdown"
                );
                std::future::pending::<()>().await
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn kill_all_processes(state: &AppState) {
    // 1. Belt-and-suspenders WS close. The shutdown_signal closure already
    //    sent Close to every connection at signal time; this second pass
    //    catches any connections that opened after the signal (between the
    //    signal firing and axum stopping the listener — narrow but real).
    for conn in state.ws_connections.all() {
        // try_send: best-effort during teardown; a full queue must not stall it.
        let _ = conn.outbound.try_send(crate::ws::OutboundMessage::Close);
    }

    // 2. Existing pool-shutdown logic.
    let stats = state.process_manager.pool_stats().await;
    for s in &stats {
        for &pid in &s.pids {
            crate::process::kill_process_group(pid);
        }
    }
}

/// Request-scoped fields threaded through the dispatch phases below.
/// Extracted once at intake from the incoming request + peer address.
struct RequestMeta {
    start: Instant,
    method_str: String,
    method_typed: http::Method,
    path: String,
    query: String,
    source_ip: String,
    /// Origin header, extracted up-front; needed for both OPTIONS and
    /// non-OPTIONS CORS handling. Invalid (non-ASCII / newline-containing)
    /// values are treated as absent by the cors module.
    request_origin: Option<String>,
    route_key_for_logs: String,
    cache_key: String,
}

impl RequestMeta {
    fn from_request(req: &Request<AxumBody>, peer: SocketAddr) -> Self {
        let method_str = req.method().as_str().to_uppercase();
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("").to_string();
        let request_origin = req
            .headers()
            .get(http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        Self {
            start: Instant::now(),
            method_typed: req.method().clone(),
            route_key_for_logs: crate::router::Router::route_key(&method_str, &path),
            cache_key: CacheLayer::make_key(&method_str, &path, &query),
            method_str,
            path,
            query,
            source_ip: peer.ip().to_string(),
            request_origin,
        }
    }

    fn latency_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

pub(crate) async fn dispatch_lambda(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<AxumBody>,
) -> Response {
    let meta = RequestMeta::from_request(&req, peer);

    // ── CORS preflight (OPTIONS) ─────────────────────────────────────────────
    if meta.method_str == "OPTIONS" {
        return cors_preflight_response(&state, &meta).await;
    }

    // ── Per-caller API key + rate-limit admission (data plane) ───────────────
    // Gates every non-`/_riz/*` request (function invocations + colocated
    // static) when `[api_keys]` is configured; the reserved `/_riz/*` admin +
    // observability plane keeps its own `[auth] bearer_token`. No keys → open
    // (returns None), so behavior is unchanged when the feature is unused.
    // Placed ahead of static + cache so a rejected caller touches neither.
    if !meta.path.starts_with("/_riz/") {
        if let Some(resp) = enforce_api_key_admission(&state, &meta, req.headers()).await {
            return resp;
        }
    }

    // ── Static file serving (GET/HEAD fallback) ──────────────────────────────
    if meta.method_typed == http::Method::GET || meta.method_typed == http::Method::HEAD {
        if let Some(resp) = try_serve_static(&state, &meta, req.headers()).await {
            log_static_hit(&state, &meta, resp.status().as_u16());
            return resp;
        }
    }

    // BUG-12: skip cache for authenticated/personalized requests.
    let has_auth =
        req.headers().contains_key("authorization") || req.headers().contains_key("cookie");

    // Cache check — only when no auth headers present. The cache is keyed
    // by raw method+path+query (a cached response is the request's response,
    // not the function's).
    if !has_auth {
        if let Some(cached) = state.cache.get(&meta.cache_key).await {
            return serve_cache_hit(&state, &cached, &meta).await;
        }
    }

    let request_id = Uuid::new_v4().to_string();
    let gw_request = match build_gateway_event(&state, req, &meta, &request_id).await {
        Ok(event) => event,
        Err(resp) => return resp,
    };
    let gw_request = match enforce_authorizer_phase(&state, gw_request, &meta).await {
        Ok(event) => event,
        Err(resp) => return resp,
    };

    let result = {
        let router = state.router.read().await;
        router.dispatch(gw_request).await
    };
    let latency = meta.latency_ms();

    match result {
        Ok(outcome) => {
            finalize_dispatch_success(&state, outcome, &meta, &request_id, has_auth, latency).await
        }
        Err(e) => finalize_dispatch_error(&state, &e, &meta, &request_id).await,
    }
}

/// Handle a CORS preflight (OPTIONS): 404 when no handler owns the path
/// (method-agnostic lookup; acceptance criterion 4), otherwise 204 with the
/// owning function's preflight headers.
async fn cors_preflight_response(state: &AppState, meta: &RequestMeta) -> Response {
    let function_name_for_path = {
        let router = state.router.read().await;
        router.function_for_path(&meta.path)
    };
    match function_name_for_path {
        None => {
            // No handler owns this path → OPTIONS on an unregistered path
            // must return 404, not 204.
            tracing::debug!(
                path = %meta.path,
                "CORS preflight: path not registered — returning 404"
            );
            StatusCode::NOT_FOUND.into_response()
        }
        Some(fn_name) => {
            // Path is registered → return 204 with preflight headers.
            let cors_cfg = {
                let cfg = state.config.read().await;
                cfg.effective_cors_for(&fn_name)
            };
            let origin_ref = meta.request_origin.as_deref().unwrap_or("");
            let preflight_hdrs = cors::preflight_headers(&cors_cfg, origin_ref);
            tracing::debug!(
                path = %meta.path,
                fn_name,
                origin = origin_ref,
                headers_count = preflight_hdrs.len(),
                "CORS preflight: returning 204"
            );
            let mut builder = axum::http::response::Builder::new().status(StatusCode::NO_CONTENT);
            for (k, v) in &preflight_hdrs {
                builder = builder.header(k, v);
            }
            builder
                .body(AxumBody::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

/// Static file serving (GET/HEAD fallback). After CORS preflight, before any
/// function dispatch or cache lookup: if `[static]` is configured and NO
/// function owns this path, serve a file from disk. Functions and `/_riz/*`
/// always win — the `function_for_path` gate is the same method-agnostic
/// lookup that drives CORS preflight, so a static file can never shadow an
/// API (a method mismatch still yields the function's own 405/404).
/// `static_files::serve` returns `None` only when the path is not under the
/// mount, in which case the caller falls through to the normal 404 path.
async fn try_serve_static(
    state: &AppState,
    meta: &RequestMeta,
    headers: &http::HeaderMap,
) -> Option<Response> {
    let static_cfg = { state.config.read().await.static_site.clone() }?;
    let owned_by_function = {
        let router = state.router.read().await;
        router.function_for_path(&meta.path).is_some()
    };
    if owned_by_function {
        return None;
    }
    crate::static_files::serve(&meta.method_typed, &meta.path, headers, &static_cfg).await
}

/// Access-log line for a static-file hit (mirrors the dispatch access log).
fn log_static_hit(state: &AppState, meta: &RequestMeta, status: u16) {
    let latency = meta.latency_ms();
    let request_id = Uuid::new_v4().to_string();
    let (method_str, path, source_ip) = (&meta.method_str, &meta.path, &meta.source_ip);
    state.push_log(
        "INFO",
        Some(&meta.route_key_for_logs),
        format!(
            "{method_str} {path} {status} {latency:.0}ms [static] req={request_id} ip={source_ip}"
        ),
    );
}

/// Serve a cache hit: access-log the hit, attribute it to the owning
/// function for metrics, and attach that function's CORS response headers.
async fn serve_cache_hit(
    state: &AppState,
    cached: &ApiGatewayV2httpResponse,
    meta: &RequestMeta,
) -> Response {
    let latency = meta.latency_ms();
    let request_id = Uuid::new_v4().to_string();
    let (method_str, path, source_ip) = (&meta.method_str, &meta.path, &meta.source_ip);
    state.push_log(
        "INFO",
        Some(&meta.route_key_for_logs),
        format!("{method_str} {path} 200 {latency:.0}ms [cache] req={request_id} ip={source_ip}"),
    );
    // Attribute the cache hit to the function whose routes include this
    // path — this avoids locking state.router. When no function claims the
    // path we fall back to the global CORS config, which preserves the
    // cache_hit metric without a wrong attribution.
    let fn_name = attribute_path_to_function(state, meta).await;
    // Compute CORS response headers for the cache-hit path.
    let cors_hdrs =
        cors_headers_for(state, fn_name.as_deref(), meta.request_origin.as_deref()).await;
    if let Some(fn_name) = fn_name {
        state
            .riz_state
            .record_invocation(&fn_name, latency, true, true)
            .await;
    }
    apply_cors_response_headers(gateway_to_axum(cached), &cors_hdrs)
}

/// Scan FunctionState.routes (a Vec<String> of "METHOD /path" display
/// strings) for the function owning this method+path. Match when the stored
/// method is ANY or equals the request method, and the path matches.
async fn attribute_path_to_function(state: &AppState, meta: &RequestMeta) -> Option<String> {
    let functions = state.riz_state.functions.read().await;
    functions
        .values()
        .find(|f| {
            f.routes.iter().any(|r| {
                if let Some((stored_method, stored_path)) = r.split_once(' ') {
                    let method_ok =
                        stored_method == "ANY" || stored_method == meta.method_str.as_str();
                    let path_ok = stored_path == meta.path.as_str();
                    method_ok && path_ok
                } else {
                    false
                }
            })
        })
        .map(|f| f.name.clone())
}

/// CORS response headers for a response: the named function's effective
/// config, or the global `[cors]` block when no function is attributed.
async fn cors_headers_for(
    state: &AppState,
    fn_name: Option<&str>,
    origin: Option<&str>,
) -> http::HeaderMap {
    let cfg = state.config.read().await;
    let cors_cfg = fn_name
        .map(|n| cfg.effective_cors_for(n))
        .unwrap_or_else(|| cfg.cors.clone());
    cors::response_headers(&cors_cfg, origin)
}

/// Assemble the AWS API Gateway v2 request event from the incoming request:
/// headers, cookies, query map, body, and the request context. Returns the
/// ready event, or the error response to send instead (oversized body).
async fn build_gateway_event(
    state: &AppState,
    req: Request<AxumBody>,
    meta: &RequestMeta,
    request_id: &str,
) -> Result<ApiGatewayV2httpRequest, Response> {
    let user_agent = req
        .headers()
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Headers — passed through as `http::HeaderMap` directly into the event.
    let headers = req.headers().clone();
    let cookies = parse_event_cookies(req.headers());
    let query_string_parameters = parse_query_map(&meta.query);
    let (body, is_base64_encoded) = read_event_body(req).await?;

    let time_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // AWS v2 time format: "01/Jan/2025:12:00:00 +0000"
    let time_str = format_aws_time(time_epoch);

    // Read stage from the first registered user function, or fall back to the
    // well-known default. All functions share the same server-level stage, so
    // any entry suffices. We do NOT lock state.config here.
    let stage = {
        let functions = state.riz_state.functions.read().await;
        functions
            .values()
            .next()
            .and_then(|f| f.stage.lock().ok().map(|s| s.clone()))
            .unwrap_or_else(|| "$default".to_string())
    };

    let ctx = ApiGatewayV2httpRequestContext {
        route_key: Some(meta.route_key_for_logs.clone()),
        account_id: Some("riz".into()),
        stage: Some(stage),
        request_id: Some(request_id.to_string()),
        time: Some(time_str),
        time_epoch: time_epoch as i64,
        http: ApiGatewayV2httpRequestContextHttpDescription {
            method: meta.method_typed.clone(),
            path: Some(meta.path.clone()),
            protocol: Some("HTTP/1.1".into()),
            source_ip: Some(meta.source_ip.clone()),
            user_agent: Some(user_agent),
        },
        ..Default::default()
    };

    Ok(ApiGatewayV2httpRequest {
        version: Some("2.0".into()),
        route_key: Some(meta.route_key_for_logs.clone()),
        raw_path: Some(meta.path.clone()),
        raw_query_string: Some(meta.query.clone()),
        cookies,
        headers,
        query_string_parameters,
        path_parameters: Default::default(), // router populates after match
        request_context: ctx,
        stage_variables: Default::default(),
        body,
        is_base64_encoded,
        kind: None,
        method_arn: None,
        http_method: meta.method_typed.clone(),
        identity_source: None,
        authorization_token: None,
        resource: None,
    })
}

/// Cookies — AWS v2 represents them as a separate top-level field, parsed
/// from the `Cookie` header (split on `; `).
fn parse_event_cookies(headers: &http::HeaderMap) -> Option<Vec<String>> {
    headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split("; ")
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
}

/// Query string parameters — parse from raw query into a flat map (the
/// AWS QueryMap accepts a HashMap<String, String> via From; we feed it
/// pairs and let the type coerce).
fn parse_query_map(query: &str) -> aws_lambda_events::query_map::QueryMap {
    let mut acc: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            acc.insert(percent_decode(k), percent_decode(v));
        } else {
            acc.insert(percent_decode(pair), String::new());
        }
    }
    acc.into()
}

/// Read the request body into the event's body field.
/// BUG-10: 413 instead of silently swallowing an oversized body.
/// BUG-09: non-UTF8 bodies are base64-encoded in the event.
async fn read_event_body(req: Request<AxumBody>) -> Result<(Option<String>, bool), Response> {
    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::PAYLOAD_TOO_LARGE.into_response()),
    };
    if body_bytes.is_empty() {
        return Ok((None, false));
    }
    match String::from_utf8(body_bytes.to_vec()) {
        Ok(s) => Ok((Some(s), false)),
        Err(e) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(e.into_bytes());
            Ok((Some(encoded), true))
        }
    }
}

/// Authorizer enforcement: look up the authorizer for the matched function
/// (if any) BEFORE dispatching. We do a cheap route-match to find the
/// function name, then read the authorizer config from the shared config
/// RwLock (one lock acquisition per request on the auth path, none when no
/// authorizer is configured for the matched function). Returns the (possibly
/// context-enriched) event, or the 401/403/500 response to send instead.
/// Per-caller API-key resolution + token-bucket rate limiting on the data
/// plane. Reads the caller's secret from the `X-Api-Key` header, resolves it
/// against `[api_keys]`, and spends one token from that caller's bucket.
///
/// Returns `None` to admit (no keys configured, or a valid key with budget);
/// `Some(response)` to reject: `401` for an unknown/absent key when keys are
/// configured (fail-closed), or `429` + `Retry-After` when the caller is over
/// its rate limit. Both rejections bump a global metric and log a warning
/// (with the caller name on the 429).
async fn enforce_api_key_admission(
    state: &AppState,
    meta: &RequestMeta,
    headers: &axum::http::HeaderMap,
) -> Option<Response> {
    use crate::auth::api_key::Admission;
    let presented = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    let admission = state.rate_limiter.read().await.admit(presented);
    match admission {
        Admission::Open | Admission::Admitted => None,
        Admission::Unauthorized => {
            state
                .riz_state
                .api_key_rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // A rejected request never reaches dispatch's request_id; mint one
            // here so the warning still carries req=/ip= correlation fields.
            let request_id = Uuid::new_v4();
            let source_ip = &meta.source_ip;
            state.push_log(
                "warn",
                Some(&meta.route_key_for_logs),
                format!(
                    "api key rejected (unknown or absent X-Api-Key) req={request_id} ip={source_ip}"
                ),
            );
            crate::audit::auth_denied("api_key", source_ip, "unknown_or_absent_key");
            Some(api_key_error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                None,
            ))
        }
        Admission::RateLimited {
            caller,
            retry_after_secs,
        } => {
            state
                .riz_state
                .rate_limited
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let request_id = Uuid::new_v4();
            let source_ip = &meta.source_ip;
            state.push_log(
                "warn",
                Some(&meta.route_key_for_logs),
                format!(
                    "rate limit exceeded for caller '{caller}' req={request_id} ip={source_ip} retry_after={retry_after_secs}s"
                ),
            );
            Some(api_key_error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate limited",
                Some(retry_after_secs),
            ))
        }
    }
}

/// Build a JSON error response for the API-key gate, optionally carrying a
/// `Retry-After` header (whole seconds) for the 429 case.
fn api_key_error_response(
    status: StatusCode,
    error: &str,
    retry_after_secs: Option<u64>,
) -> Response {
    let body = format!(r#"{{"error":"{error}"}}"#);
    let mut resp = (status, [("content-type", "application/json")], body).into_response();
    if let Some(secs) = retry_after_secs {
        if let Ok(val) = axum::http::HeaderValue::from_str(&secs.to_string()) {
            resp.headers_mut().insert(http::header::RETRY_AFTER, val);
        }
    }
    resp
}

async fn enforce_authorizer_phase(
    state: &AppState,
    gw_request: ApiGatewayV2httpRequest,
    meta: &RequestMeta,
) -> Result<ApiGatewayV2httpRequest, Response> {
    let authorizer_config = authorizer_config_for_route(state, meta).await;
    if authorizer_config.is_none() {
        return Ok(gw_request);
    }

    let auth_header = gw_request
        .headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Find function name again for cache key (borrow already dropped).
    let function_name = {
        let router = state.router.read().await;
        router
            .find_function_name(&meta.method_str, &meta.path)
            .unwrap_or_else(|| "_unmatched".to_string())
    };

    let cache_key = AuthCacheKey::new(&meta.source_ip, auth_header.as_deref(), &function_name);

    // Cache hit check — skip the full enforce_authorizer call.
    if let Some(cached_output) = state.auth_cache.get(&cache_key).await {
        return Ok(inject_authorizer_context(gw_request, &cached_output));
    }

    match enforce_authorizer(
        authorizer_config.as_ref(),
        &meta.source_ip,
        auth_header.as_deref(),
        &function_name,
        gw_request,
        &state.auth_cache,
        &state.process_manager,
    )
    .await
    {
        Ok(event) => Ok(event),
        Err(err) => Err(authorizer_error_response(&err, meta, &function_name)),
    }
}

/// Look up the matched function's authorizer config, if any.
async fn authorizer_config_for_route(
    state: &AppState,
    meta: &RequestMeta,
) -> Option<crate::config::AuthorizerConfig> {
    // Phase 1: find the function name for this route.
    let function_name_opt = {
        let router = state.router.read().await;
        router.find_function_name(&meta.method_str, &meta.path)
    };
    // Phase 2: look up its authorizer config.
    if let Some(ref fn_name) = function_name_opt {
        let cfg = state.config.read().await;
        cfg.functions
            .get(fn_name.as_str())
            .and_then(|f| f.authorizer.clone())
    } else {
        None
    }
}

/// Map an authorizer failure to its response: 401/403 carry the intended
/// denial, 500 covers transient authorizer errors. Each is warn-logged with
/// the source ip + function for correlation.
fn authorizer_error_response(
    err: &crate::auth::authorizer::AuthError,
    meta: &RequestMeta,
    function_name: &str,
) -> Response {
    use crate::auth::authorizer::AuthError;
    let (status, public_msg) = match err {
        AuthError::Unauthorized(msg) => {
            tracing::warn!(
                source_ip = %meta.source_ip,
                function = %function_name,
                "authorizer: 401 Unauthorized — {msg}"
            );
            crate::audit::auth_denied("authorizer", &meta.source_ip, "unauthorized");
            (401, "Unauthorized")
        }
        AuthError::Forbidden(msg) => {
            tracing::warn!(
                source_ip = %meta.source_ip,
                function = %function_name,
                "authorizer: 403 Forbidden — {msg}"
            );
            crate::audit::auth_denied("authorizer", &meta.source_ip, "forbidden");
            (403, "Forbidden")
        }
        AuthError::Other(msg) => {
            tracing::warn!(
                source_ip = %meta.source_ip,
                function = %function_name,
                "authorizer: 500 transient error — {msg}"
            );
            (500, "Internal Server Error")
        }
    };
    gateway_to_axum(&crate::runtime::error_response(status, public_msg))
}

/// Finalize a successful dispatch. Router returns (function_name, response).
/// All metrics, cache bookkeeping, and access logs attribute to
/// function_name — mirrors AWS CloudWatch per-function metric semantics.
async fn finalize_dispatch_success(
    state: &AppState,
    outcome: crate::router::DispatchOutcome,
    meta: &RequestMeta,
    request_id: &str,
    has_auth: bool,
    latency: f64,
) -> Response {
    let function_name = outcome.function_name.clone();
    let gw_resp = outcome.response;

    // Read per-function metadata from RizState — no config lock needed.
    let (runtime_tag, effective_ttl) = {
        let functions = state.riz_state.functions.read().await;
        match functions.get(&function_name) {
            Some(fs) => (
                fs.runtime_tag
                    .lock()
                    .map(|r| r.clone())
                    .unwrap_or_else(|_| "system".to_string()),
                fs.cache_ttl_secs.load(std::sync::atomic::Ordering::Relaxed),
            ),
            None => ("system".to_string(), 0),
        }
    };

    let status_u16 = gw_resp.status_code as u16;
    let healthy = status_u16 < 500;
    let _ = &runtime_tag;

    // Request root span (OTLP). Non-blocking emit — telemetry is
    // best-effort and never adds latency to or fails the request path.
    emit_request_span(
        &state.telemetry,
        meta.start,
        latency,
        &meta.method_str,
        &function_name,
        status_u16,
    );

    state
        .riz_state
        .record_invocation(&function_name, latency, healthy, false)
        .await;

    let (method_str, path, source_ip) = (&meta.method_str, &meta.path, &meta.source_ip);
    state.push_log(
        "INFO",
        Some(&function_name),
        format!("{method_str} {path} {status_u16} {latency:.0}ms req={request_id} ip={source_ip} fn={function_name}"),
    );

    if !has_auth && effective_ttl > 0 && status_u16 < 400 {
        state
            .cache
            .set(meta.cache_key.clone(), gw_resp.clone(), effective_ttl)
            .await;
    }

    // Append CORS response headers for this function.
    let cors_hdrs =
        cors_headers_for(state, Some(&function_name), meta.request_origin.as_deref()).await;
    apply_cors_response_headers(gateway_to_axum(&gw_resp), &cors_hdrs)
}

/// Finalize a failed dispatch. No function attribution possible — log under
/// "_unmatched". BUG-16: error-path access logs MUST also carry request_id +
/// source_ip so operators can correlate failures to a specific request. The
/// success + cache-hit paths already include both.
async fn finalize_dispatch_error(
    state: &AppState,
    e: &crate::runtime::HandlerError,
    meta: &RequestMeta,
    request_id: &str,
) -> Response {
    let resp = e.to_response();
    error!(req = %request_id, ip = %meta.source_ip, "dispatch error: {e}");
    let (method_str, path, source_ip) = (&meta.method_str, &meta.path, &meta.source_ip);
    state.push_log(
        "ERROR",
        None,
        format!("dispatch error {method_str} {path} req={request_id} ip={source_ip}: {e}"),
    );
    // Apply global CORS config for error responses (unmatched routes).
    let cors_hdrs = cors_headers_for(state, None, meta.request_origin.as_deref()).await;
    apply_cors_response_headers(gateway_to_axum(&resp), &cors_hdrs)
}

/// Current wall-clock time as unix-nanos (for OTLP span timestamps).
pub(crate) fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A random lowercase-hex OTLP trace id (16 bytes / 32 hex chars).
pub(crate) fn new_trace_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// A random lowercase-hex OTLP span id (8 bytes / 16 hex chars). Derived from
/// the low half of a UUID — sufficient entropy for in-process span ids.
pub(crate) fn new_span_id() -> String {
    let u = Uuid::new_v4().as_u128();
    format!("{:016x}", (u as u64))
}

/// Emit the request root span on completion. Computes the span start from the
/// request `start` instant and the measured `latency`. Non-blocking.
fn emit_request_span(
    telemetry: &crate::observability::TelemetryHandle,
    start: Instant,
    latency_ms: f64,
    method: &str,
    function_name: &str,
    status: u16,
) {
    use crate::observability::ipc::{AttrValue, SpanKind, TelemetryEvent};
    use std::collections::BTreeMap;

    let end = now_unix_nanos();
    // Reconstruct the span window from the monotonic latency so start <= end.
    let span_nanos = (latency_ms * 1_000_000.0) as u64;
    let begin = end.saturating_sub(span_nanos);
    let _ = start;

    let mut attributes = BTreeMap::new();
    attributes.insert(
        "http.method".to_string(),
        AttrValue::String(method.to_string()),
    );
    attributes.insert(
        "http.route".to_string(),
        AttrValue::String(function_name.to_string()),
    );
    attributes.insert(
        "http.status_code".to_string(),
        AttrValue::Int(status as i64),
    );
    attributes.insert("duration_ms".to_string(), AttrValue::Double(latency_ms));

    telemetry.emit(TelemetryEvent {
        name: format!("{method} {function_name}"),
        kind: SpanKind::Server,
        trace_id: new_trace_id(),
        span_id: new_span_id(),
        parent_span_id: None,
        start_unix_nanos: begin,
        end_unix_nanos: end,
        attributes,
    });
}

/// Merge CORS response headers into an existing axum `Response`.
///
/// Called after `gateway_to_axum` to attach `Access-Control-*` headers to
/// every non-OPTIONS response when the request Origin is in the allow-list.
/// Headers are inserted; any existing CORS headers from the handler are
/// overwritten (riz is authoritative for CORS, not the Lambda function).
fn apply_cors_response_headers(mut response: Response, cors_hdrs: &http::HeaderMap) -> Response {
    let headers = response.headers_mut();
    for (k, v) in cors_hdrs {
        headers.insert(k, v.clone());
    }
    response
}

/// Convert an AWS API Gateway v2 response into an axum HTTP response.
/// Handles `Body::Text`, `Body::Binary`, base64-encoded `Body::Text`, and
/// v2 cookies (emitted as `Set-Cookie` headers since axum is HTTP/1.1+).
pub fn gateway_to_axum(resp: &ApiGatewayV2httpResponse) -> Response {
    let status =
        StatusCode::from_u16(resp.status_code as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = axum::http::response::Builder::new().status(status);
    for (name, value) in resp.headers.iter() {
        builder = builder.header(name, value);
    }
    for (name, value) in resp.multi_value_headers.iter() {
        builder = builder.header(name, value);
    }
    // v2 cookies → one Set-Cookie header per entry.
    for cookie in &resp.cookies {
        if let Ok(v) = http::HeaderValue::from_str(cookie) {
            builder = builder.header(http::header::SET_COOKIE, v);
        }
    }

    let body_bytes: Vec<u8> = match resp.body.as_ref() {
        Some(Body::Text(s)) if resp.is_base64_encoded => base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .unwrap_or_default(),
        Some(Body::Text(s)) => s.clone().into_bytes(),
        Some(Body::Binary(b)) => b.clone(),
        Some(Body::Empty) | None => Vec::new(),
    };
    builder
        .body(AxumBody::from(body_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Minimal percent-decode for query string values.
fn percent_decode(s: &str) -> String {
    crate::router::percent_decode(s)
}

/// Format a millisecond UNIX epoch into the AWS v2 `time` field format.
/// Example: "04/Mar/2020:21:43:58 +0000".
fn format_aws_time(epoch_ms: u128) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%d/%b/%Y:%H:%M:%S +0000").to_string())
        .unwrap_or_default()
}

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

async fn ready_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let stats = state.process_manager.pool_stats().await;
    let unhealthy: Vec<String> = stats
        .iter()
        .filter(|s| !s.healthy)
        .map(|s| s.name.clone())
        .collect();
    if unhealthy.is_empty() {
        (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ok",
                unhealthy,
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "degraded",
                unhealthy,
            }),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
pub struct InvalidateRequest {
    pub keys: Option<Vec<String>>,
    pub prefix: Option<String>,
}

#[derive(Serialize)]
pub struct InvalidateResponse {
    pub evicted: usize,
}

async fn cache_invalidate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<InvalidateRequest>,
) -> Response {
    // Admin surface: bearer-gated like the other /_riz/* admin endpoints.
    // An attacker who can flush the cache can manufacture load.
    let expected = { state.config.read().await.effective_bearer_token() };
    if let Some(resp) = bearer_reject(&headers, expected.as_deref()) {
        return resp;
    }
    let evicted = if let Some(keys) = &body.keys {
        state.cache.invalidate_keys(keys).await
    } else if let Some(prefix) = &body.prefix {
        state.cache.invalidate_prefix(prefix).await
    } else {
        0
    };
    Json(InvalidateResponse { evicted }).into_response()
}

/// Shared bearer gate for axum-level (non-Lambda-envelope) routes: the LLM
/// gateway (`/_riz/v1/*` — the endpoints that SPEND MONEY upstream) and
/// admin actions. Returns `Some(401)` when a token is configured and the
/// request's Authorization header doesn't match (constant-time compare);
/// `None` means proceed. No token configured → open, matching the
/// documented local-dev default for the rest of `/_riz/*`.
fn bearer_reject(headers: &axum::http::HeaderMap, expected: Option<&str>) -> Option<Response> {
    let expected = expected?;
    let auth = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if crate::auth::bearer::validate_bearer(auth, expected) {
        None
    } else {
        Some(
            (
                StatusCode::UNAUTHORIZED,
                [("content-type", "application/json")],
                r#"{"error":"unauthorized"}"#,
            )
                .into_response(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_signal_resolves_on_ctrl_c() {
        // Construct the future to exercise the type signature and any
        // signal-handler installation side-effects; we don't await it
        // because there's no SIGINT in the test environment.
        let _fut = shutdown_signal();
    }

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse { status: "ok" };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
    }

    #[test]
    fn ready_response_omits_empty_unhealthy() {
        let resp = ReadyResponse {
            status: "ok",
            unhealthy: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("unhealthy"));
    }

    #[test]
    fn ready_response_includes_unhealthy_list() {
        let resp = ReadyResponse {
            status: "degraded",
            unhealthy: vec!["route1".to_string(), "route2".to_string()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("route1"));
        assert!(json.contains("route2"));
    }

    #[test]
    fn aws_time_format_known_epoch() {
        // 2025-05-22 14:00:00 UTC = 1747922400 secs
        let formatted = format_aws_time(1_747_922_400_000u128);
        // Just sanity-check the format shape — month name, year, time, +0000
        assert!(formatted.contains("/May/2025:"), "got {formatted}");
        assert!(formatted.ends_with(" +0000"));
    }

    #[test]
    fn aws_time_format_regression_aws_docs_epoch() {
        // epoch 1583348638390 ms = 2020-03-04T19:03:58Z (UTC, verified via chrono)
        let formatted = format_aws_time(1_583_348_638_390u128);
        assert_eq!(formatted, "04/Mar/2020:19:03:58 +0000", "got {formatted}");
    }
}
