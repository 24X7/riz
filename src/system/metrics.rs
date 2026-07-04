//! /_riz/metrics handler — emits Prometheus text format 0.0.4.

use crate::auth::bearer::validate_bearer;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::runtime::{
    response::{json_response, text_response},
    HandlerError, LambdaHandler, RouteEntry, RouteMethod,
};
use crate::state::{FunctionKind, RizState};
use async_trait::async_trait;
use std::fmt::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct MetricsHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
    bearer_token: Option<String>,
}

impl MetricsHandler {
    pub fn new(riz_state: Arc<RizState>, bearer_token: Option<String>) -> Self {
        Self {
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/_riz/metrics".into(),
            }],
            riz_state,
            bearer_token,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

type FunctionsMap = indexmap::IndexMap<String, Arc<crate::state::FunctionState>>;

/// One `# HELP`/`# TYPE counter` header plus one sample per user function.
fn write_counter_section(
    out: &mut String,
    functions: &FunctionsMap,
    name: &str,
    help: &str,
    value: impl Fn(&crate::state::FunctionState) -> u64,
) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    for (_, f) in functions.iter() {
        if matches!(f.kind, FunctionKind::System) {
            continue;
        }
        let _ = writeln!(out, "{name}{{function=\"{}\"}} {}", esc(&f.name), value(f));
    }
}

fn write_latency_section(out: &mut String, functions: &FunctionsMap, now: std::time::Instant) {
    let _ = writeln!(
        out,
        "# HELP riz_latency_ms Function latency percentiles over 5-min window"
    );
    let _ = writeln!(out, "# TYPE riz_latency_ms summary");
    for (_, f) in functions.iter() {
        if matches!(f.kind, FunctionKind::System) {
            continue;
        }
        let (p50, p75, p90, p95, p99) = f
            .latency
            .lock()
            .map(|mut w| w.percentiles(now))
            .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
        let route = esc(&f.name);
        for (quantile, value) in [
            ("0.5", p50),
            ("0.75", p75),
            ("0.9", p90),
            ("0.95", p95),
            ("0.99", p99),
        ] {
            let _ = writeln!(
                out,
                "riz_latency_ms{{function=\"{route}\",quantile=\"{quantile}\"}} {value}"
            );
        }
    }
}

fn write_health_section(out: &mut String, functions: &FunctionsMap) {
    let _ = writeln!(
        out,
        "# HELP riz_function_healthy 1 if pool healthy, 0 otherwise"
    );
    let _ = writeln!(out, "# TYPE riz_function_healthy gauge");
    for (_, f) in functions.iter() {
        if matches!(f.kind, FunctionKind::System) {
            continue;
        }
        let v = if f.healthy.load(Ordering::Relaxed) {
            1
        } else {
            0
        };
        let _ = writeln!(
            out,
            "riz_function_healthy{{function=\"{}\"}} {}",
            esc(&f.name),
            v
        );
    }
}

#[async_trait]
impl LambdaHandler for MetricsHandler {
    fn name(&self) -> &str {
        "GET /_riz/metrics"
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        if let Some(expected) = &self.bearer_token {
            let auth_header = event
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            if !validate_bearer(auth_header, expected) {
                let path = event.raw_path.as_deref().unwrap_or("/_riz/metrics");
                let ip = event
                    .request_context
                    .http
                    .source_ip
                    .as_deref()
                    .unwrap_or("-");
                tracing::warn!(path = %path, source_ip = %ip, "unauthorized request");
                return Ok(json_response(
                    401,
                    &serde_json::json!({"error": "unauthorized"}),
                ));
            }
        }
        let now = std::time::Instant::now();
        let functions = self.riz_state.functions.read().await;
        let mut out = String::with_capacity(4096);

        write_counter_section(
            &mut out,
            &functions,
            "riz_invocations_total",
            "Total function invocations",
            |f| f.invocations.load(Ordering::Relaxed),
        );
        write_counter_section(
            &mut out,
            &functions,
            "riz_errors_total",
            "Total function errors",
            |f| f.errors.load(Ordering::Relaxed),
        );
        write_latency_section(&mut out, &functions, now);
        write_counter_section(
            &mut out,
            &functions,
            "riz_cold_starts_total",
            "Process spawns",
            |f| f.cold_starts.load(Ordering::Relaxed),
        );
        write_health_section(&mut out, &functions);

        let _ = writeln!(out, "# HELP riz_uptime_seconds Runtime uptime");
        let _ = writeln!(out, "# TYPE riz_uptime_seconds gauge");
        let _ = writeln!(out, "riz_uptime_seconds {}", self.riz_state.uptime_secs());

        Ok(text_response(200, "text/plain; version=0.0.4", out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Body;
    use crate::state::FunctionState;
    use crate::test_helpers::make_event;

    fn evt() -> ApiGatewayV2httpRequest {
        make_event("GET", "/_riz/metrics")
    }

    fn evt_with_auth(token: &str) -> ApiGatewayV2httpRequest {
        let mut e = make_event("GET", "/_riz/metrics");
        e.headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        e
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
            env: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        };
        FunctionState::user("api", c, "$default", 0)
    }

    #[tokio::test]
    async fn metrics_returns_401_when_token_required_and_missing() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, Some("secret".into()));
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn metrics_returns_401_when_token_required_and_wrong() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, Some("secret".into()));
        let resp = h.invoke(evt_with_auth("wrong")).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn metrics_returns_200_when_token_required_and_correct() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, Some("secret".into()));
        let resp = h.invoke(evt_with_auth("secret")).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn metrics_returns_200_when_no_token_configured() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_text() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
        let ct = resp
            .headers
            .get(http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("text/plain; version=0.0.4"));
    }

    #[tokio::test]
    async fn metrics_emits_help_and_type_lines() {
        let s = Arc::new(RizState::new());
        let h = MetricsHandler::new(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        let body = body_text(&resp);
        assert!(body.contains("# HELP riz_invocations_total"));
        assert!(body.contains("# TYPE riz_invocations_total counter"));
        assert!(body.contains("# TYPE riz_latency_ms summary"));
        assert!(body.contains("riz_uptime_seconds"));
    }

    #[tokio::test]
    async fn metrics_includes_user_function_counters() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        s.record_invocation("api", 5.0, true, false).await;
        s.record_invocation("api", 10.0, false, false).await;
        let h = MetricsHandler::new(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        let body = body_text(&resp);
        assert!(
            body.contains("riz_invocations_total{function=\"api\"} 2"),
            "body was:\n{body}"
        );
        assert!(body.contains("riz_errors_total{function=\"api\"} 1"));
    }

    #[tokio::test]
    async fn metrics_excludes_system_functions() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system(
            "_riz_health",
            vec!["GET /_riz/health".into()],
            "$default",
        ))
        .await;
        let h = MetricsHandler::new(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        let body = body_text(&resp);
        assert!(
            !body.contains("_riz_health"),
            "system functions must not appear in metrics"
        );
    }
}
