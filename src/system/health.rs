//! /_riz/health handler — returns 200 with runtime + per-function status.

use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::runtime::{
    response::json_response, HandlerError, LambdaHandler, RouteEntry, RouteMethod,
};
use crate::state::{FunctionKind, RizState};
use async_trait::async_trait;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
    functions: Vec<FunctionHealth>,
}

#[derive(Serialize)]
struct FunctionHealth {
    name: String,
    routes: Vec<String>,
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
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/_riz/health".into(),
            }],
            riz_state,
        }
    }
}

#[async_trait]
impl LambdaHandler for HealthHandler {
    fn name(&self) -> &str {
        "GET /_riz/health"
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        _event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<FunctionHealth> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) {
                // System endpoints excluded from health body to avoid recursive noise.
                continue;
            }
            let (p50, _, _, _, p99) = f
                .latency
                .lock()
                .map(|mut w| w.percentiles(now))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
            let last_secs = f
                .last_invoked
                .lock()
                .ok()
                .and_then(|l| l.map(|t| now.duration_since(t).as_secs_f64()));
            out.push(FunctionHealth {
                name: f.name.clone(),
                routes: f.routes.clone(),
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
        Ok(json_response(200, &body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Body;
    use crate::state::FunctionState;
    use crate::test_helpers::make_event;

    fn evt() -> ApiGatewayV2httpRequest {
        make_event("GET", "/_riz/health")
    }

    fn body_text(resp: &ApiGatewayV2httpResponse) -> String {
        match resp.body.as_ref().expect("body") {
            Body::Text(s) => s.clone(),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    fn user_state() -> FunctionState {
        let c = crate::config::FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./h.ts"),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
        };
        FunctionState::user("api", c, "$default", 0)
    }

    #[tokio::test]
    async fn health_returns_200_with_ok_status() {
        let s = Arc::new(RizState::new());
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["status"], "ok");
        assert!(body["functions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn health_includes_registered_user_functions() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0]["name"], "api");
        assert_eq!(functions[0]["healthy"], true);
    }

    #[tokio::test]
    async fn health_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system(
            "_riz_health",
            vec!["GET /_riz/health".into()],
            "$default",
        ))
        .await;
        s.register(user_state()).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let functions = body["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1, "system functions must be excluded");
        assert_eq!(functions[0]["name"], "api");
    }

    #[tokio::test]
    async fn health_reflects_recorded_invocations() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        s.record_invocation("api", 10.0, true, false).await;
        s.record_invocation("api", 20.0, true, false).await;
        let h = HealthHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert_eq!(body["functions"][0]["invocations"], 2);
    }
}
