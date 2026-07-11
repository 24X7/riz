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
    process_manager: Arc<crate::process::ProcessManager>,
    bearer_token: Option<String>,
    /// When false, `/_riz/metrics` returns 404 (the `[metrics] enabled = false`
    /// control). The route is still mounted so the surface is discoverable.
    enabled: bool,
}

impl MetricsHandler {
    pub fn new(
        riz_state: Arc<RizState>,
        process_manager: Arc<crate::process::ProcessManager>,
        bearer_token: Option<String>,
        enabled: bool,
    ) -> Self {
        Self {
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/_riz/metrics".into(),
            }],
            riz_state,
            process_manager,
            bearer_token,
            enabled,
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
        if !self.enabled {
            // Operator disabled metrics ([metrics] enabled = false).
            return Ok(json_response(
                404,
                &serde_json::json!({"error": "metrics disabled"}),
            ));
        }
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
        // Cache efficiency — already tracked per function, now surfaced.
        write_counter_section(
            &mut out,
            &functions,
            "riz_cache_hits_total",
            "Response-cache hits",
            |f| f.cache_hits.load(Ordering::Relaxed),
        );
        write_counter_section(
            &mut out,
            &functions,
            "riz_cache_misses_total",
            "Response-cache misses",
            |f| f.cache_misses.load(Ordering::Relaxed),
        );
        write_health_section(&mut out, &functions);
        drop(functions);

        // Saturation + worker reliability — the load-and-supervision signals a
        // warm-pool runtime lives or dies by (see docs/METRICS.md).
        write_pool_sections(&mut out, &self.process_manager.pool_stats().await);

        let _ = writeln!(out, "# HELP riz_uptime_seconds Runtime uptime");
        let _ = writeln!(out, "# TYPE riz_uptime_seconds gauge");
        let _ = writeln!(out, "riz_uptime_seconds {}", self.riz_state.uptime_secs());

        // Build/version info — the conventional single-sample identity metric.
        let _ = writeln!(
            out,
            "# HELP riz_build_info Build identity (constant 1; read the labels)"
        );
        let _ = writeln!(out, "# TYPE riz_build_info gauge");
        let _ = writeln!(
            out,
            "riz_build_info{{version=\"{}\"}} 1",
            esc(self.riz_state.version)
        );

        Ok(text_response(200, "text/plain; version=0.0.4", out))
    }
}

/// Saturation + supervision metrics, one sample per function pool. These come
/// from `ProcessManager::pool_stats` (a live snapshot), not the per-function
/// counters. Cardinality stays bounded: one series per pool per metric.
fn write_pool_sections(out: &mut String, pools: &[crate::process::PoolStats]) {
    let gauge = |out: &mut String, name: &str, help: &str| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} gauge");
    };

    gauge(
        out,
        "riz_concurrency_limit",
        "Configured concurrency permits",
    );
    for p in pools {
        let _ = writeln!(
            out,
            "riz_concurrency_limit{{function=\"{}\"}} {}",
            esc(&p.name),
            p.concurrency
        );
    }

    gauge(
        out,
        "riz_concurrency_in_use",
        "Permits held now (saturation: in_use/limit is utilization)",
    );
    for p in pools {
        let _ = writeln!(
            out,
            "riz_concurrency_in_use{{function=\"{}\"}} {}",
            esc(&p.name),
            p.concurrency_in_use
        );
    }

    gauge(out, "riz_workers", "Live worker processes in the pool");
    for p in pools {
        let _ = writeln!(
            out,
            "riz_workers{{function=\"{}\"}} {}",
            esc(&p.name),
            p.pids.len()
        );
    }

    gauge(
        out,
        "riz_worker_consecutive_crashes",
        "Crashes since last success (proximity to the crash-loop breaker)",
    );
    for p in pools {
        let _ = writeln!(
            out,
            "riz_worker_consecutive_crashes{{function=\"{}\"}} {}",
            esc(&p.name),
            p.consecutive_crashes
        );
    }

    gauge(
        out,
        "riz_pool_memory_bytes",
        "Resident memory across the pool",
    );
    for p in pools {
        let bytes = (p.memory_rss_mb * 1024.0 * 1024.0).round() as u64;
        let _ = writeln!(
            out,
            "riz_pool_memory_bytes{{function=\"{}\"}} {}",
            esc(&p.name),
            bytes
        );
    }

    let _ = writeln!(
        out,
        "# HELP riz_worker_restarts_total Worker respawns (crash or timeout)"
    );
    let _ = writeln!(out, "# TYPE riz_worker_restarts_total counter");
    for p in pools {
        let _ = writeln!(
            out,
            "riz_worker_restarts_total{{function=\"{}\"}} {}",
            esc(&p.name),
            p.restart_count
        );
    }

    let _ = writeln!(
        out,
        "# HELP riz_admission_rejected_total Requests load-shed at the concurrency limit"
    );
    let _ = writeln!(out, "# TYPE riz_admission_rejected_total counter");
    for p in pools {
        let _ = writeln!(
            out,
            "riz_admission_rejected_total{{function=\"{}\"}} {}",
            esc(&p.name),
            p.admission_rejected
        );
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

    /// Build a MetricsHandler with an (empty) real ProcessManager and metrics
    /// enabled — the common shape for these unit tests.
    fn mk(s: Arc<RizState>, bearer: Option<String>) -> MetricsHandler {
        let pm = Arc::new(crate::process::ProcessManager::new(s.clone()));
        MetricsHandler::new(s, pm, bearer, true)
    }

    #[tokio::test]
    async fn metrics_returns_401_when_token_required_and_missing() {
        let s = Arc::new(RizState::new());
        let h = mk(s, Some("secret".into()));
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn metrics_returns_401_when_token_required_and_wrong() {
        let s = Arc::new(RizState::new());
        let h = mk(s, Some("secret".into()));
        let resp = h.invoke(evt_with_auth("wrong")).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn metrics_returns_200_when_token_required_and_correct() {
        let s = Arc::new(RizState::new());
        let h = mk(s, Some("secret".into()));
        let resp = h.invoke(evt_with_auth("secret")).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn metrics_returns_200_when_no_token_configured() {
        let s = Arc::new(RizState::new());
        let h = mk(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn metrics_disabled_returns_404() {
        let s = Arc::new(RizState::new());
        let pm = Arc::new(crate::process::ProcessManager::new(s.clone()));
        let h = MetricsHandler::new(s, pm, None, false);
        let resp = h.invoke(evt()).await.unwrap();
        assert_eq!(
            resp.status_code, 404,
            "[metrics] enabled = false must remove the endpoint"
        );
    }

    #[tokio::test]
    async fn metrics_emit_saturation_reliability_and_build_info() {
        let s = Arc::new(RizState::new());
        let h = mk(s, None);
        let body = body_text(&h.invoke(evt()).await.unwrap());
        // Saturation signals (the warm-pool essentials).
        assert!(body.contains("# TYPE riz_concurrency_limit gauge"));
        assert!(body.contains("# TYPE riz_concurrency_in_use gauge"));
        assert!(body.contains("# TYPE riz_admission_rejected_total counter"));
        // Worker reliability.
        assert!(body.contains("# TYPE riz_worker_restarts_total counter"));
        assert!(body.contains("# TYPE riz_worker_consecutive_crashes gauge"));
        assert!(body.contains("# TYPE riz_workers gauge"));
        // Cache efficiency + build identity.
        assert!(body.contains("# TYPE riz_cache_hits_total counter"));
        assert!(body.contains("# TYPE riz_cache_misses_total counter"));
        assert!(body.contains("riz_build_info{version="));
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_text() {
        let s = Arc::new(RizState::new());
        let h = mk(s, None);
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
        let h = mk(s, None);
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
        let h = mk(s, None);
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
        let h = mk(s, None);
        let resp = h.invoke(evt()).await.unwrap();
        let body = body_text(&resp);
        assert!(
            !body.contains("_riz_health"),
            "system functions must not appear in metrics"
        );
    }
}
