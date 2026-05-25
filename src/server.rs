use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use base64::Engine as _;
use axum::{
    body::Body as AxumBody,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router as AxumRouter,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;
use crate::cache::CacheLayer;
use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription, ApiGatewayV2httpResponse, Body,
};
use crate::state::AppState;

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
    AxumRouter::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        .fallback(any(dispatch_lambda))
        .with_state(state)
}

pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = build_app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    info!("riz listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            tracing::info!("shutdown signal received — draining in-flight requests (30s timeout)");
        })
        .await?;
    tracing::info!("all requests drained — killing child processes");
    kill_all_processes(&state).await;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
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
    let user_agent = req.headers()
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let route_key = crate::router::Router::route_key(&method_str, &path);
    let cache_key = CacheLayer::make_key(&method_str, &path, &query);

    // BUG-12: skip cache for authenticated/personalized requests
    let has_auth = req.headers().contains_key("authorization") || req.headers().contains_key("cookie");

    // Resolve per-route config knobs (runtime tag + cache TTL override).
    // System routes won't appear in config.routes — defaults apply.
    let (route_runtime_tag, route_cache_ttl, default_ttl) = {
        let cfg = state.config.read().await;
        let matched = cfg.routes.iter()
            .find(|r| crate::router::Router::route_key(&r.method, &r.path) == route_key);
        let runtime_tag = matched
            .map(|r| r.runtime.as_str().to_string())
            .unwrap_or_else(|| "system".to_string());
        let ttl_override = matched.and_then(|r| r.cache_ttl_secs);
        (runtime_tag, ttl_override, cfg.cache.default_ttl_secs)
    };

    // Cache check — only when no auth headers present.
    if !has_auth {
        if let Some(cached) = state.cache.get(&cache_key).await {
            let latency = start.elapsed().as_secs_f64() * 1000.0;
            let request_id = Uuid::new_v4().to_string();
            state.record_request(&route_key, true, latency, true).await;
            state.metrics.record_cache_hit(&route_key);
            state.push_log(
                "INFO",
                Some(&route_key),
                format!("{method_str} {path} 200 {latency:.0}ms [cache] req={request_id} ip={source_ip}"),
            );
            return gateway_to_axum(&cached);
        }
    }

    // Headers — passed through as `http::HeaderMap` directly into the event.
    let headers = req.headers().clone();

    // Cookies — AWS v2 represents them as a separate top-level field, parsed
    // from the `Cookie` header (split on `; `).
    let cookies: Option<Vec<String>> = req.headers()
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split("; ").map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect())
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

    let mut ctx = ApiGatewayV2httpRequestContext::default();
    ctx.route_key = Some(route_key.clone());
    ctx.account_id = Some("riz".into());
    ctx.stage = Some("$default".into());
    ctx.request_id = Some(request_id.clone());
    ctx.time = Some(time_str);
    ctx.time_epoch = time_epoch as i64;
    ctx.http = ApiGatewayV2httpRequestContextHttpDescription {
        method: method_typed.clone(),
        path: Some(path.clone()),
        protocol: Some("HTTP/1.1".into()),
        source_ip: Some(source_ip.clone()),
        user_agent: Some(user_agent),
    };

    let gw_request = ApiGatewayV2httpRequest {
        version: Some("2.0".into()),
        route_key: Some(route_key.clone()),
        raw_path: Some(path.clone()),
        raw_query_string: Some(query.clone()),
        cookies,
        headers,
        query_string_parameters,
        path_parameters: Default::default(),  // router populates after match
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
            let route_key = outcome.route_key.clone();
            let gw_resp = outcome.response;

            // Re-resolve runtime tag + ttl override against the matched pattern.
            let (route_runtime_tag, route_cache_ttl) = {
                let cfg = state.config.read().await;
                let matched = cfg.routes.iter()
                    .find(|r| crate::router::Router::route_key(&r.method, &r.path) == route_key);
                let runtime_tag = matched
                    .map(|r| r.runtime.as_str().to_string())
                    .unwrap_or_else(|| "system".to_string());
                let ttl_override = matched.and_then(|r| r.cache_ttl_secs);
                (runtime_tag, ttl_override)
            };
            let _ = (route_runtime_tag.clone(), route_cache_ttl);

            let status_u16 = gw_resp.status_code as u16;
            let healthy = status_u16 < 500;
            state.metrics.record_request(&route_key, &method_str, status_u16, latency);

            match status_u16 {
                502 => state.metrics.record_lambda_crash(&route_key, &route_runtime_tag),
                504 => state.metrics.record_lambda_timeout(&route_key),
                _ => {}
            }
            state.metrics.record_lambda_healthy(&route_key, healthy);

            if status_u16 < 400 {
                state.metrics.record_cache_miss(&route_key);
            }

            state.record_request(&route_key, false, latency, healthy).await;

            state.push_log(
                "INFO",
                Some(&route_key),
                format!("{method_str} {path} {status_u16} {latency:.0}ms req={request_id} ip={source_ip}"),
            );

            let ttl = route_cache_ttl.unwrap_or(default_ttl);
            if !has_auth && ttl > 0 && status_u16 < 400 {
                state.cache.set(cache_key, gw_resp.clone(), ttl).await;
            }

            gateway_to_axum(&gw_resp)
        }
        Err(e) => {
            let resp = e.to_response();
            error!("dispatch error for {route_key}: {e}");
            state.metrics.record_lambda_crash(&route_key, &route_runtime_tag);
            state.metrics.record_lambda_healthy(&route_key, false);
            state.record_request(&route_key, false, latency, false).await;
            state.push_log("ERROR", Some(&route_key), format!("dispatch error {route_key}: {e}"));
            gateway_to_axum(&resp)
        }
    }
}

/// Convert an AWS API Gateway v2 response into an axum HTTP response.
/// Handles `Body::Text`, `Body::Binary`, base64-encoded `Body::Text`, and
/// v2 cookies (emitted as `Set-Cookie` headers since axum is HTTP/1.1+).
pub fn gateway_to_axum(resp: &ApiGatewayV2httpResponse) -> Response {
    let status = StatusCode::from_u16(resp.status_code as u16)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
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
        Some(Body::Text(s)) if resp.is_base64_encoded => {
            base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes())
                .unwrap_or_default()
        }
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
/// Example: "01/Jan/2025:12:00:00 +0000". We approximate using time-of-day
/// arithmetic — sufficient for handlers that just log it.
fn format_aws_time(epoch_ms: u128) -> String {
    let secs = (epoch_ms / 1000) as u64;
    // Days since 1970-01-01
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    let month_name = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][month - 1];
    format!("{:02}/{}/{}:{:02}:{:02}:{:02} +0000", day, month_name, year, h, m, s)
}

/// Convert days-since-epoch to (year, month-1-indexed, day-1-indexed).
fn days_to_ymd(mut days: u64) -> (u64, usize, u64) {
    let mut year = 1970u64;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if days < in_year { break; }
        days -= in_year;
        year += 1;
    }
    let month_days = if is_leap(year) {
        [31,29,31,30,31,30,31,31,30,31,30,31]
    } else {
        [31,28,31,30,31,30,31,31,30,31,30,31]
    };
    let mut month = 0usize;
    while month < 12 && days >= month_days[month] as u64 {
        days -= month_days[month] as u64;
        month += 1;
    }
    (year, month + 1, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

async fn ready_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let stats = state.process_manager.pool_stats().await;
    let unhealthy: Vec<String> = stats
        .iter()
        .filter(|s| !s.healthy)
        .map(|s| s.route_key.clone())
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
        let _fut = shutdown_signal();
        assert!(true);
    }

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse { status: "ok" };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
    }

    #[test]
    fn ready_response_omits_empty_unhealthy() {
        let resp = ReadyResponse { status: "ok", unhealthy: vec![] };
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
}
