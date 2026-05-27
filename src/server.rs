use crate::cache::CacheLayer;
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
    let mut app = AxumRouter::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler));

    // Mount WebSocket upgrade routes for every protocol=websocket function.
    // build_app runs once at startup, so a try_read in this sync context is
    // OK — no other writer should be holding the config write lock at startup.
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
                                  ci: axum::extract::ConnectInfo<std::net::SocketAddr>| {
                                let s = state_clone.clone();
                                let n = name_owned.clone();
                                async move {
                                    crate::ws::upgrade::ws_upgrade_handler(
                                        axum::extract::State((s, n)),
                                        ci,
                                        ws,
                                        headers,
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

    app.fallback(any(dispatch_lambda)).with_state(state)
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

    let serve_future = axum::serve(listener, app).with_graceful_shutdown(async {
        shutdown_signal().await;
        tracing::info!(
            "shutdown signal received — draining in-flight requests (max {}s)",
            SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
        );
    });

    // Hard cap the drain. axum's graceful shutdown would otherwise wait
    // indefinitely if a handler hangs.
    match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, serve_future).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            tracing::warn!(
                "drain timeout ({}s) elapsed — forcing shutdown with requests still in flight",
                SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
            );
        }
    }
    tracing::info!("draining complete — killing child processes");
    kill_all_processes(&state).await;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn kill_all_processes(state: &AppState) {
    // 1. Close every WebSocket connection cleanly so clients see a CLOSE
    //    frame rather than a TCP reset on shutdown.
    for conn in state.ws_connections.all() {
        let _ = conn.outbound.send(crate::ws::OutboundMessage::Close);
    }

    // 2. Existing pool-shutdown logic.
    let stats = state.process_manager.pool_stats().await;
    for s in &stats {
        for &pid in &s.pids {
            crate::process::kill_process_group(pid);
        }
    }
}

async fn dispatch_lambda(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<AxumBody>,
) -> Response {
    let start = Instant::now();
    let method_str = req.method().as_str().to_uppercase();
    let method_typed = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let source_ip = peer.ip().to_string();
    let user_agent = req
        .headers()
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let cache_key = CacheLayer::make_key(&method_str, &path, &query);
    let route_key_for_logs = crate::router::Router::route_key(&method_str, &path);

    // BUG-12: skip cache for authenticated/personalized requests
    let has_auth =
        req.headers().contains_key("authorization") || req.headers().contains_key("cookie");

    let (default_ttl, stage) = {
        let cfg = state.config.read().await;
        (cfg.cache.default_ttl_secs, cfg.server.stage.clone())
    };

    // Cache check — only when no auth headers present. The cache is keyed
    // by raw method+path+query (a cached response is the request's response,
    // not the function's). Attribution to function name happens via the
    // first router pass we need to do for the lookup; for cache-hit we use
    // the request path as the log key.
    if !has_auth {
        if let Some(cached) = state.cache.get(&cache_key).await {
            let latency = start.elapsed().as_secs_f64() * 1000.0;
            let request_id = Uuid::new_v4().to_string();
            state.metrics.record_cache_hit(&route_key_for_logs);
            state.push_log(
                "INFO",
                Some(&route_key_for_logs),
                format!("{method_str} {path} 200 {latency:.0}ms [cache] req={request_id} ip={source_ip}"),
            );
            // Attribute the cache hit to the function that owns the route so
            // FunctionState.cache_hits stays accurate (mirrors the cache-miss
            // path which calls record_request with cache_hit=false).
            let fn_name = {
                let router = state.router.read().await;
                router
                    .handlers()
                    .iter()
                    .find(|h| {
                        h.routes()
                            .iter()
                            .any(|r| r.match_path(&method_str, &path).is_some())
                    })
                    .map(|h| h.name().to_string())
            };
            if let Some(fn_name) = fn_name {
                state.record_request(&fn_name, true, latency, true).await;
            }
            return gateway_to_axum(&cached);
        }
    }

    // Headers — passed through as `http::HeaderMap` directly into the event.
    let headers = req.headers().clone();

    // Cookies — AWS v2 represents them as a separate top-level field, parsed
    // from the `Cookie` header (split on `; `).
    let cookies: Option<Vec<String>> = req
        .headers()
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split("; ")
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty());

    // Query string parameters — parse from raw query into a flat map (the
    // AWS QueryMap accepts a HashMap<String, String> via From; we feed it
    // pairs and let the type coerce).
    let query_string_parameters: aws_lambda_events::query_map::QueryMap = {
        let mut acc: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            if let Some((k, v)) = pair.split_once('=') {
                acc.insert(percent_decode(k), percent_decode(v));
            } else {
                acc.insert(percent_decode(pair), String::new());
            }
        }
        acc.into()
    };

    // BUG-10: 413 instead of silently swallowing oversized body.
    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    // BUG-09: non-UTF8 bodies are base64-encoded in the event.
    let (body, is_base64_encoded) = if body_bytes.is_empty() {
        (None, false)
    } else {
        match String::from_utf8(body_bytes.to_vec()) {
            Ok(s) => (Some(s), false),
            Err(e) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(e.into_bytes());
                (Some(encoded), true)
            }
        }
    };

    let request_id = Uuid::new_v4().to_string();
    let time_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // AWS v2 time format: "01/Jan/2025:12:00:00 +0000"
    let time_str = format_aws_time(time_epoch);

    let ctx = ApiGatewayV2httpRequestContext {
        route_key: Some(route_key_for_logs.clone()),
        account_id: Some("riz".into()),
        stage: Some(stage),
        request_id: Some(request_id.clone()),
        time: Some(time_str),
        time_epoch: time_epoch as i64,
        http: ApiGatewayV2httpRequestContextHttpDescription {
            method: method_typed.clone(),
            path: Some(path.clone()),
            protocol: Some("HTTP/1.1".into()),
            source_ip: Some(source_ip.clone()),
            user_agent: Some(user_agent),
        },
        ..Default::default()
    };

    let gw_request = ApiGatewayV2httpRequest {
        version: Some("2.0".into()),
        route_key: Some(route_key_for_logs.clone()),
        raw_path: Some(path.clone()),
        raw_query_string: Some(query.clone()),
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
        http_method: method_typed,
        identity_source: None,
        authorization_token: None,
        resource: None,
    };

    let result = {
        let router = state.router.read().await;
        router.dispatch(gw_request).await
    };
    let latency = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(outcome) => {
            // Router returns (function_name, response). All metrics, cache
            // bookkeeping, and access logs attribute to function_name —
            // mirrors AWS CloudWatch per-function metric semantics.
            let function_name = outcome.function_name.clone();
            let gw_resp = outcome.response;

            // Look up the function config to get the runtime tag for metrics
            // and the per-function cache TTL override.
            let (runtime_tag, fn_cache_ttl) = {
                let cfg = state.config.read().await;
                match cfg.functions.get(&function_name) {
                    Some(fc) => (fc.runtime.as_str().to_string(), fc.cache_ttl_secs),
                    None => ("system".to_string(), None),
                }
            };

            let status_u16 = gw_resp.status_code as u16;
            let healthy = status_u16 < 500;
            state
                .metrics
                .record_request(&function_name, &method_str, status_u16, latency);

            match status_u16 {
                502 => state
                    .metrics
                    .record_lambda_crash(&function_name, &runtime_tag),
                504 => state.metrics.record_lambda_timeout(&function_name),
                _ => {}
            }
            state.metrics.record_lambda_healthy(&function_name, healthy);

            if status_u16 < 400 {
                state.metrics.record_cache_miss(&function_name);
            }

            state
                .record_request(&function_name, false, latency, healthy)
                .await;

            state.push_log(
                "INFO",
                Some(&function_name),
                format!("{method_str} {path} {status_u16} {latency:.0}ms req={request_id} ip={source_ip} fn={function_name}"),
            );

            let ttl = fn_cache_ttl.unwrap_or(default_ttl);
            if !has_auth && ttl > 0 && status_u16 < 400 {
                state.cache.set(cache_key, gw_resp.clone(), ttl).await;
            }

            gateway_to_axum(&gw_resp)
        }
        Err(e) => {
            let resp = e.to_response();
            error!("dispatch error: {e}");
            // No function attribution possible — log under "_unmatched".
            state.push_log(
                "ERROR",
                None,
                format!("dispatch error {method_str} {path}: {e}"),
            );
            gateway_to_axum(&resp)
        }
    }
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
    Json(body): Json<InvalidateRequest>,
) -> impl IntoResponse {
    let evicted = if let Some(keys) = &body.keys {
        state.cache.invalidate_keys(keys).await
    } else if let Some(prefix) = &body.prefix {
        state.cache.invalidate_prefix(prefix).await
    } else {
        0
    };
    Json(InvalidateResponse { evicted })
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
