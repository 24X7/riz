//! /_riz/metrics handler — emits Prometheus text format 0.0.4.

use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

pub struct MetricsHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl MetricsHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/metrics".into() }],
            riz_state,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

#[async_trait]
impl LambdaHandler for MetricsHandler {
    fn name(&self) -> &str { "GET /_riz/metrics" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out = String::with_capacity(4096);

        let _ = writeln!(out, "# HELP riz_invocations_total Total function invocations");
        let _ = writeln!(out, "# TYPE riz_invocations_total counter");
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.invocations.load(Ordering::Relaxed);
            let _ = writeln!(out, "riz_invocations_total{{route=\"{}\"}} {}", esc(&f.route_key), n);
        }

        let _ = writeln!(out, "# HELP riz_errors_total Total function errors");
        let _ = writeln!(out, "# TYPE riz_errors_total counter");
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.errors.load(Ordering::Relaxed);
            let _ = writeln!(out, "riz_errors_total{{route=\"{}\"}} {}", esc(&f.route_key), n);
        }

        let _ = writeln!(out, "# HELP riz_latency_ms Function latency percentiles over 5-min window");
        let _ = writeln!(out, "# TYPE riz_latency_ms summary");
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let (p50, p75, p90, p95, p99) = f.latency.lock()
                .map(|mut w| w.percentiles(now))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
            let route = esc(&f.route_key);
            let _ = writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.5\"}} {}", route, p50);
            let _ = writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.75\"}} {}", route, p75);
            let _ = writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.9\"}} {}", route, p90);
            let _ = writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.95\"}} {}", route, p95);
            let _ = writeln!(out, "riz_latency_ms{{route=\"{}\",quantile=\"0.99\"}} {}", route, p99);
        }

        let _ = writeln!(out, "# HELP riz_cold_starts_total Process spawns");
        let _ = writeln!(out, "# TYPE riz_cold_starts_total counter");
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let n = f.cold_starts.load(Ordering::Relaxed);
            let _ = writeln!(out, "riz_cold_starts_total{{route=\"{}\"}} {}", esc(&f.route_key), n);
        }

        let _ = writeln!(out, "# HELP riz_function_healthy 1 if pool healthy, 0 otherwise");
        let _ = writeln!(out, "# TYPE riz_function_healthy gauge");
        for (_, f) in functions.iter() {
            if matches!(f.kind, FunctionKind::System) { continue; }
            let v = if f.healthy.load(Ordering::Relaxed) { 1 } else { 0 };
            let _ = writeln!(out, "riz_function_healthy{{route=\"{}\"}} {}", esc(&f.route_key), v);
        }

        let _ = writeln!(out, "# HELP riz_uptime_seconds Runtime uptime");
        let _ = writeln!(out, "# TYPE riz_uptime_seconds gauge");
        let _ = writeln!(out, "riz_uptime_seconds {}", self.riz_state.uptime_secs());

        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "text/plain; version=0.0.4".into());
        Ok(GatewayResponse {
            status_code: 200,
            headers: Some(headers),
            body: Some(out),
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
            route_key: "GET /_riz/metrics".into(),
            raw_path: "/_riz/metrics".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/_riz/metrics".into(),
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
        let r = crate::config::RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./h.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_text() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let ct = resp.headers.unwrap().get("content-type").unwrap().clone();
        assert!(ct.starts_with("text/plain; version=0.0.4"));
    }

    #[tokio::test]
    async fn metrics_emits_help_and_type_lines() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(body.contains("# HELP riz_invocations_total"));
        assert!(body.contains("# TYPE riz_invocations_total counter"));
        assert!(body.contains("# TYPE riz_latency_ms summary"));
        assert!(body.contains("riz_uptime_seconds"));
    }

    #[tokio::test]
    async fn metrics_includes_user_function_counters() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        s.record_invocation("GET /api", 5.0, true, false).await;
        s.record_invocation("GET /api", 10.0, false, false).await;
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(body.contains("riz_invocations_total{route=\"GET /api\"} 2"), "body was:\n{body}");
        assert!(body.contains("riz_errors_total{route=\"GET /api\"} 1"));
    }

    #[tokio::test]
    async fn metrics_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        let h = MetricsHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body = resp.body.unwrap();
        assert!(!body.contains("/_riz/health"), "system functions must not appear in metrics");
    }
}
