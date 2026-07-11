//! `/_riz/ready` — Kubernetes-style readiness probe, distinct from liveness.
//!
//! `/_riz/health` is a **liveness** signal: it answers 200 whenever the process
//! is up, so an orchestrator uses it to decide whether to *restart* the pod.
//! Readiness answers a different question — "should this instance receive
//! traffic right now?" — and must go unready the instant a graceful shutdown
//! begins, so the load balancer stops routing new requests to a draining
//! instance while in-flight ones finish. It flips to 503 as soon as
//! [`crate::tui::SHUTDOWN_REQUESTED`] is set (the drain path sets it in
//! `server.rs`), and reports 200 while serving.

use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::runtime::{
    response::json_response, HandlerError, LambdaHandler, RouteEntry, RouteMethod,
};
use async_trait::async_trait;
use serde::Serialize;
use std::sync::atomic::Ordering;

#[derive(Serialize)]
struct ReadyBody {
    status: &'static str,
}

pub struct ReadinessHandler {
    routes: Vec<RouteEntry>,
}

impl ReadinessHandler {
    pub fn new() -> Self {
        Self {
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/_riz/ready".into(),
            }],
        }
    }
}

impl Default for ReadinessHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LambdaHandler for ReadinessHandler {
    fn name(&self) -> &str {
        "GET /_riz/ready"
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        _event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        if crate::tui::SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            // Draining: shed new traffic while in-flight requests finish.
            return Ok(json_response(503, &ReadyBody { status: "draining" }));
        }
        Ok(json_response(200, &ReadyBody { status: "ready" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Body;
    use crate::test_helpers::make_event;
    use crate::tui::SHUTDOWN_REQUESTED;

    fn body_str(resp: &ApiGatewayV2httpResponse) -> String {
        match &resp.body {
            Some(Body::Text(t)) => t.clone(),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    // SHUTDOWN_REQUESTED is process-global; this test owns it and restores it,
    // and is the only readiness test, so it can assert both states in sequence.
    #[tokio::test]
    async fn ready_when_serving_draining_after_shutdown_signal() {
        let handler = ReadinessHandler::new();
        let prior = SHUTDOWN_REQUESTED.load(Ordering::Relaxed);

        // Serving: 200 ready.
        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
        let resp = handler
            .invoke(make_event("GET", "/_riz/ready"))
            .await
            .expect("readiness invoke");
        assert_eq!(resp.status_code, 200);
        assert!(body_str(&resp).contains("ready"));

        // Draining: 503 so the load balancer stops sending new traffic.
        SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
        let resp = handler
            .invoke(make_event("GET", "/_riz/ready"))
            .await
            .expect("readiness invoke");
        assert_eq!(resp.status_code, 503);
        assert!(body_str(&resp).contains("draining"));

        SHUTDOWN_REQUESTED.store(prior, Ordering::Relaxed);
    }
}
