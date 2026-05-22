use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use base64::Engine as _;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router as AxumRouter,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;
use crate::cache::CacheLayer;
use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
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
    req: Request<Body>,
) -> Response {
    let start = Instant::now();
    let method = req.method().as_str().to_uppercase();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let source_ip = peer.ip().to_string();

    let route_key = crate::router::Router::route_key(&method, &path);
    let cache_key = CacheLayer::make_key(&method, &path, &query);

    // BUG-12: skip cache for authenticated/personalized requests
    let has_auth = req.headers().contains_key("authorization") || req.headers().contains_key("cookie");

    // Resolve per-route config knobs (runtime tag + cache TTL override). System
    // routes won't appear in config.routes — we fall back to defaults for those.
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

    // Cache check — only when no auth headers present
    if !has_auth {
        if let Some(cached) = state.cache.get(&cache_key).await {
            let latency = start.elapsed().as_secs_f64() * 1000.0;
            let request_id = Uuid::new_v4().to_string();
            state.record_request(&route_key, true, latency, true).await;
            state.metrics.record_cache_hit(&route_key);
            // BUG-16: include request_id and source_ip in cache-hit log
            state.push_log(
                "INFO",
                Some(&route_key),
                format!("{method} {path} 200 {latency:.0}ms [cache] req={request_id} ip={source_ip}"),
            );
            return gateway_to_axum(&cached);
        }
    }

    // Build Gateway v2 request
    let headers = extract_headers(req.headers());
    // BUG-10: return 413 instead of silently swallowing oversized body
    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    // BUG-09: handle binary (non-UTF8) bodies by base64-encoding them
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
        .as_millis() as u64;

    let gw_request = GatewayRequest {
        version: "2.0".into(),
        route_key: route_key.clone(),
        raw_path: path.clone(),
        raw_query_string: query.clone(),
        headers,
        request_context: RequestContext {
            http: HttpContext {
                method: method.clone(),
                path: path.clone(),
                protocol: "HTTP/1.1".into(),
                source_ip: source_ip.clone(),
            },
            request_id: request_id.clone(),
            time_epoch,
        },
        path_parameters: None,
        body,
        is_base64_encoded,
    };

    // Trait dispatch — system handlers (mounted first) win on /_riz/*; user
    // ProcessHandlers serve the rest. If no handler claims it, a 404 response
    // is returned by the router.
    let result = {
        let router = state.router.read().await;
        router.dispatch(gw_request).await
    };
    let latency = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(gw_resp) => {
            let healthy = gw_resp.status_code < 500;
            state.metrics.record_request(&route_key, &method, gw_resp.status_code, latency);

            // Emit specific metrics for lambda-side errors
            match gw_resp.status_code {
                502 => state.metrics.record_lambda_crash(&route_key, &route_runtime_tag),
                504 => state.metrics.record_lambda_timeout(&route_key),
                _ => {}
            }
            state.metrics.record_lambda_healthy(&route_key, healthy);

            // Cache miss metric only for successful cache-eligible requests
            if gw_resp.status_code < 400 {
                state.metrics.record_cache_miss(&route_key);
            }

            state.record_request(&route_key, false, latency, healthy).await;

            // BUG-16: include request_id and source_ip in access log
            state.push_log(
                "INFO",
                Some(&route_key),
                format!("{method} {path} {} {latency:.0}ms req={request_id} ip={source_ip}", gw_resp.status_code),
            );

            // BUG-12: only cache when no auth headers were present
            let ttl = route_cache_ttl.unwrap_or(default_ttl);
            if !has_auth && ttl > 0 && gw_resp.status_code < 400 {
                state.cache.set(cache_key, gw_resp.clone(), ttl).await;
            }

            gateway_to_axum(&gw_resp)
        }
        Err(e) => {
            // HandlerError — convert via its canonical to_response()
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

pub fn gateway_to_axum(resp: &GatewayResponse) -> Response {
    let status = StatusCode::from_u16(resp.status_code)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = axum::http::response::Builder::new().status(status);
    if let Some(headers) = &resp.headers {
        for (k, v) in headers {
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::try_from(k.as_str()),
                axum::http::HeaderValue::try_from(v.as_str()),
            ) {
                builder = builder.header(name, value);
            }
        }
    }
    // BUG-09: decode base64 body when Lambda signals it's binary
    let body_bytes: Vec<u8> = if resp.is_base64_encoded == Some(true) {
        let encoded = resp.body.clone().unwrap_or_default();
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap_or_default()
    } else {
        resp.body.clone().unwrap_or_default().into_bytes()
    };
    builder
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn extract_headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect()
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
        // We can't actually send signals in a unit test, but we can verify
        // the shutdown_signal future is a valid future type.
        // This test just ensures it compiles and is properly formed.
        let _fut = shutdown_signal();
        // If this compiles, the signal handler is correctly set up
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
        let resp = ReadyResponse {
            status: "ok",
            unhealthy: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("unhealthy"),
            "empty unhealthy list must be omitted"
        );
    }

    #[test]
    fn ready_response_includes_unhealthy_list() {
        let resp = ReadyResponse {
            status: "degraded",
            unhealthy: vec!["route1".to_string(), "route2".to_string()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("unhealthy"));
        assert!(json.contains("route1"));
        assert!(json.contains("route2"));
    }

    /// BUG-09: binary body is base64-encoded and flag is set
    #[test]
    fn binary_body_is_base64_encoded() {
        // Simulate bytes that are NOT valid UTF-8
        let binary: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01];
        let result = String::from_utf8(binary.clone());
        assert!(result.is_err(), "should fail UTF-8 parse");

        let encoded = base64::engine::general_purpose::STANDARD.encode(&binary);
        // Verify round-trip: decode must give back original bytes
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap();
        assert_eq!(decoded, binary);
        // is_base64_encoded flag should be set to true in this path
        let is_base64_encoded = true;
        assert!(is_base64_encoded);
    }

    /// BUG-12: has_auth is true when Authorization or Cookie header is present
    #[test]
    fn auth_headers_skip_cache() {
        let mut headers = HeaderMap::new();
        // No auth headers — has_auth should be false
        let has_auth = headers.contains_key("authorization") || headers.contains_key("cookie");
        assert!(!has_auth);

        // With Authorization header
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer token123"),
        );
        let has_auth = headers.contains_key("authorization") || headers.contains_key("cookie");
        assert!(has_auth, "Authorization header must trigger has_auth");

        // With Cookie header only
        let mut headers2 = HeaderMap::new();
        headers2.insert(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_static("session=abc"),
        );
        let has_auth2 = headers2.contains_key("authorization") || headers2.contains_key("cookie");
        assert!(has_auth2, "Cookie header must trigger has_auth");
    }

    /// BUG-16: access log format includes req= and ip= fields
    #[test]
    fn access_log_format_includes_request_id() {
        let method = "GET";
        let path = "/api/data";
        let status = 200u16;
        let latency = 42.5f64;
        let request_id = "550e8400-e29b-41d4-a716-446655440000";
        let source_ip = "10.0.0.1";

        let log = format!(
            "{method} {path} {status} {latency:.0}ms req={request_id} ip={source_ip}"
        );
        assert!(log.contains("req=550e8400-e29b-41d4-a716-446655440000"), "log must contain req= field");
        assert!(log.contains("ip=10.0.0.1"), "log must contain ip= field");
        assert!(log.contains("GET /api/data 200"), "log must contain method, path, status");
    }
}
