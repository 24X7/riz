use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, post},
    Json, Router as AxumRouter,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;
use crate::cache::CacheLayer;
use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
use crate::state::AppState;

pub fn build_app(state: Arc<AppState>) -> AxumRouter {
    AxumRouter::new()
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        .fallback(any(dispatch_lambda))
        .with_state(state)
}

pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = build_app(state).into_make_service_with_connect_info::<SocketAddr>();
    info!("osbox listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
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

    let (route, path_params) = {
        let router = state.router.read().await;
        match router.match_route(&method, &path) {
            Some(m) => (m.route.clone(), m.path_params.clone()),
            None => return (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    };
    let route_key = crate::router::Router::route_key(&method, &route.path);
    let cache_key = CacheLayer::make_key(&method, &path, &query);

    // Cache check
    if let Some(cached) = state.cache.get(&cache_key).await {
        let latency = start.elapsed().as_secs_f64() * 1000.0;
        state.record_request(&route_key, true, latency, true).await;
        state.metrics.record_cache_hit(&route_key);
        return gateway_to_axum(&cached);
    }

    // Build Gateway v2 request
    let headers = extract_headers(req.headers());
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let body = if body_bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&body_bytes).into_owned())
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
                source_ip,
            },
            request_id,
            time_epoch,
        },
        path_parameters: if path_params.is_empty() { None } else { Some(path_params) },
        body,
        is_base64_encoded: false,
    };

    // Invoke lambda
    let result = state.process_manager.invoke(&route_key, &gw_request, route.timeout_ms).await;
    let latency = start.elapsed().as_secs_f64() * 1000.0;

    let default_ttl = {
        let cfg = state.config.read().await;
        cfg.cache.default_ttl_secs
    };

    match result {
        Ok(gw_resp) => {
            let healthy = gw_resp.status_code < 500;
            state.metrics.record_request(&route_key, &method, gw_resp.status_code, latency);

            // Emit specific metrics for lambda-side errors
            match gw_resp.status_code {
                502 => state.metrics.record_lambda_crash(&route_key, route.runtime.as_str()),
                504 => state.metrics.record_lambda_timeout(&route_key),
                _ => {}
            }
            state.metrics.record_lambda_healthy(&route_key, healthy);

            // Cache miss metric only for successful cache-eligible requests
            if gw_resp.status_code < 400 {
                state.metrics.record_cache_miss(&route_key);
            }

            state.record_request(&route_key, false, latency, healthy).await;

            if gw_resp.status_code >= 500 {
                state.push_log("WARN", Some(&route_key), format!("lambda {} returned {}", route_key, gw_resp.status_code)).await;
            }

            let ttl = route.cache_ttl_secs.unwrap_or(default_ttl);
            if ttl > 0 && gw_resp.status_code < 400 {
                state.cache.set(cache_key, gw_resp.clone(), ttl).await;
            }

            gateway_to_axum(&gw_resp)
        }
        Err(e) => {
            error!("dispatch error for {route_key}: {e}");
            state.metrics.record_lambda_crash(&route_key, route.runtime.as_str());
            state.metrics.record_lambda_healthy(&route_key, false);
            state.record_request(&route_key, false, latency, false).await;
            state.push_log("ERROR", Some(&route_key), format!("dispatch error {route_key}: {e}")).await;
            gateway_to_axum(&GatewayResponse::error(502, "internal error"))
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
    let body = resp.body.clone().unwrap_or_default();
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn extract_headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect()
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
