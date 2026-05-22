//! /_riz/health handler — returns 200 with runtime + per-function status.

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
    functions: Vec<FunctionHealth>,
}

#[derive(Serialize)]
struct FunctionHealth {
    route_key: String,
    healthy: bool,
    invocations: u64,
    errors: u64,
    p50_ms: f64,
    p99_ms: f64,
    last_invoked_secs_ago: Option<f64>,
}

pub struct HealthHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl HealthHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/health".into() }],
            riz_state,
        }
    }
}

#[async_trait]
impl LambdaHandler for HealthHandler {
    fn name(&self) -> &str { "GET /_riz/health" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<FunctionHealth> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) {
                // System endpoints excluded from health body to avoid recursive noise.
                continue;
            }
            let (p50, _, _, _, p99) = f.latency.lock()
                .map(|mut w| w.percentiles(now))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
            let last_secs = f.last_invoked.lock()
                .ok()
                .and_then(|l| l.map(|t| now.duration_since(t).as_secs_f64()));
            out.push(FunctionHealth {
                route_key: f.route_key.clone(),
                healthy: f.healthy.load(Ordering::Relaxed),
                invocations: f.invocations.load(Ordering::Relaxed),
                errors: f.errors.load(Ordering::Relaxed),
                p50_ms: p50,
                p99_ms: p99,
                last_invoked_secs_ago: last_secs,
            });
        }
        let body = HealthBody {
            status: "ok",
            version: self.riz_state.version,
            uptime_secs: self.riz_state.uptime_secs(),
            functions: out,
        };
        let json = serde_json::to_string(&body)
            .map_err(|e| HandlerError::Internal(e.to_string()))?;
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        Ok(GatewayResponse {
            status_code: 200,
            headers: Some(headers),
            body: Some(json),
            is_base64_encoded: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{HttpContext, RequestContext};
    use crate::state::FunctionState;

    fn evt() -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /_riz/health".into(),
            raw_path: "/_riz/health".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/_riz/health".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "r".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
        }
    }

    fn user_state() -> FunctionState {
        let route = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./h.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", route)
    }

    #[tokio::test]
    async fn health_returns_200_with_ok_status() {
        let s = Arc::new(RizState::new());
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
        assert!(body["functions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn health_includes_registered_user_functions() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0]["route_key"], "GET /api");
        assert_eq!(functions[0]["healthy"], true);
    }

    #[tokio::test]
    async fn health_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        s.register(user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1, "system functions must be excluded");
        assert_eq!(functions[0]["route_key"], "GET /api");
    }

    #[tokio::test]
    async fn health_reflects_recorded_invocations() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        s.record_invocation("GET /api", 10.0, true, false).await;
        s.record_invocation("GET /api", 20.0, true, false).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["functions"][0]["invocations"], 2);
    }
}
