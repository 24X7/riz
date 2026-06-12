//! ProcessHandler — one per FUNCTION (not per route). Holds the function's
//! routes list (Vec<RouteEntry>), the function name, and an Arc<ProcessManager>
//! to delegate invocation to. The same handler is matched by the Router for
//! every route the function declares; one pool serves them all.

use crate::config::{FunctionConfig, RouteSpec};
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::process::{PoolError, ProcessManager};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ProcessHandler {
    name: String,
    routes: Vec<RouteEntry>,
    timeout_ms: u64,
    integration_timeout_ms: u64,
    /// Stage variables injected into the event before handler invocation.
    /// Matches AWS API GW v2's `stageVariables` field — per-deployment-stage
    /// config the handler reads at runtime.
    stage_variables: std::collections::HashMap<String, String>,
    /// Pool name of the pre-invoke WASM guard, when configured
    /// (`{name}::guard_in`). See `process::guard` for the verdict contract.
    guard_in_pool: Option<String>,
    /// Pool name of the post-invoke WASM guard (`{name}::guard_out`).
    guard_out_pool: Option<String>,
    process_manager: Arc<ProcessManager>,
}

impl ProcessHandler {
    pub fn for_function(
        name: &str,
        cfg: &FunctionConfig,
        process_manager: Arc<ProcessManager>,
    ) -> Self {
        let routes: Vec<RouteEntry> = cfg
            .effective_routes(name)
            .into_iter()
            .map(|RouteSpec { path, method }| RouteEntry {
                method: RouteMethod::parse_lenient(&method),
                path,
            })
            .collect();
        Self {
            name: name.to_string(),
            routes,
            timeout_ms: cfg.timeout_ms,
            integration_timeout_ms: cfg.integration_timeout_ms,
            stage_variables: cfg.stage_variables.clone(),
            guard_in_pool: cfg
                .guard_in
                .as_ref()
                .map(|_| format!("{name}{}", crate::process::guard::GUARD_IN_SUFFIX)),
            guard_out_pool: cfg
                .guard_out
                .as_ref()
                .map(|_| format!("{name}{}", crate::process::guard::GUARD_OUT_SUFFIX)),
            process_manager,
        }
    }

    /// Run a guard pool against a JSON payload and interpret the verdict.
    /// EVERY failure path (pool error, unhealthy pool, garbage verdict,
    /// unknown action) fails CLOSED — a configured policy that can't run
    /// must never silently allow traffic.
    async fn run_guard(
        &self,
        pool_name: &str,
        payload: &serde_json::Value,
    ) -> Result<crate::process::guard::GuardVerdict, HandlerError> {
        use crate::process::guard::{GuardVerdict, GUARD_TIMEOUT_MS};
        let started = std::time::Instant::now();
        let verdict: Result<GuardVerdict, PoolError> = self
            .process_manager
            .invoke_generic(pool_name, payload, GUARD_TIMEOUT_MS)
            .await;
        let ok = matches!(
            &verdict,
            Ok(v) if v.action == "allow" || v.action == "deny"
        );
        // Guard timing surfaces in /_riz/health under the guard pool name
        // (no-op unless the name is registered, which main.rs does).
        self.process_manager
            .riz_state()
            .record_invocation(
                pool_name,
                started.elapsed().as_secs_f64() * 1000.0,
                ok,
                false,
            )
            .await;
        match verdict {
            Ok(v) if v.action == "allow" || v.action == "deny" => Ok(v),
            Ok(v) => Err(HandlerError::Process(format!(
                "guard '{pool_name}' verdict not understood (action={:?}) — failing closed",
                v.action
            ))),
            Err(e) => Err(HandlerError::Process(format!(
                "guard '{pool_name}' failed ({e}) — failing closed"
            ))),
        }
    }

    #[allow(dead_code)]
    pub fn function_name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl LambdaHandler for ProcessHandler {
    fn name(&self) -> &str {
        &self.name
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        mut event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        // Inject this function's stage variables before the handler sees the event.
        for (k, v) in &self.stage_variables {
            event.stage_variables.insert(k.clone(), v.clone());
        }

        // ── Pre-invoke WASM guard (v1 roadmap #3) ────────────────────────
        // The guard sees the event before the handler. allow → proceed
        // (optionally with a mutated event); deny → answer with the guard's
        // status without ever invoking the handler. Failures fail closed
        // (run_guard). One guard wraps every runtime alike.
        if let Some(guard_pool) = &self.guard_in_pool {
            let payload = serde_json::to_value(&event)
                .map_err(|e| HandlerError::Internal(format!("event serialize: {e}")))?;
            let verdict = self.run_guard(guard_pool, &payload).await?;
            match verdict.action.as_str() {
                "allow" => {
                    if let Some(mutated) = verdict.event {
                        event = serde_json::from_value(mutated).map_err(|e| {
                            HandlerError::Process(format!(
                                "guard '{guard_pool}' returned an invalid mutated event \
                                 ({e}) — failing closed"
                            ))
                        })?;
                    }
                }
                "deny" => {
                    let status = verdict.status_code.unwrap_or(403);
                    let body = verdict
                        .body
                        .unwrap_or_else(|| r#"{"error":"rejected by guard"}"#.to_string());
                    return Ok(crate::runtime::response::text_response(
                        status,
                        "application/json",
                        body,
                    ));
                }
                _ => unreachable!("run_guard only passes allow/deny through"),
            }
        }

        // Two timeouts here, matching AWS:
        // - integration_timeout_ms: wraps the whole call. If exceeded, we
        //   return 504 to the client without waiting for the handler.
        // - timeout_ms: enforced INSIDE process_manager.invoke; if exceeded,
        //   the child process is killed and respawned.
        let invoke = self
            .process_manager
            .invoke(&self.name, &event, self.timeout_ms);
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(self.integration_timeout_ms),
            invoke,
        )
        .await;

        let mut resp = match outcome {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                return match e {
                    PoolError::Timeout(_, ms) => Err(HandlerError::Timeout(ms)),
                    PoolError::SemaphoreExhausted(_) => {
                        Err(HandlerError::Overloaded(self.timeout_ms as usize))
                    }
                    PoolError::SemaphoreClosed(name) => {
                        Err(HandlerError::Internal(format!("pool closed: {name}")))
                    }
                    PoolError::NoPool(name) => Err(HandlerError::Internal(format!(
                        "function not configured: {name}"
                    ))),
                    PoolError::InvalidResponse(_, detail) => {
                        Err(HandlerError::Process(format!("bad gateway: {detail}")))
                    }
                    PoolError::Other(_, err) => Err(HandlerError::Process(err.to_string())),
                }
            }
            Err(_elapsed) => return Err(HandlerError::Timeout(self.integration_timeout_ms)),
        };

        // ── Post-invoke WASM guard (v1 roadmap #4) ───────────────────────
        // The guard sees the handler's response envelope before bytes leave:
        // allow passes through, `response` replaces it (redaction / shape
        // enforcement), deny swaps in status+body. Infra errors above bypass
        // this — guard_out polices handler RESPONSES, not host failures.
        if let Some(guard_pool) = &self.guard_out_pool {
            let payload = serde_json::to_value(&resp)
                .map_err(|e| HandlerError::Internal(format!("response serialize: {e}")))?;
            let verdict = self.run_guard(guard_pool, &payload).await?;
            match verdict.action.as_str() {
                "allow" => {
                    if let Some(replacement) = verdict.response {
                        resp = serde_json::from_value(replacement).map_err(|e| {
                            HandlerError::Process(format!(
                                "guard '{guard_pool}' returned an invalid replacement \
                                 response ({e}) — failing closed"
                            ))
                        })?;
                    }
                }
                "deny" => {
                    let status = verdict.status_code.unwrap_or(403);
                    let body = verdict
                        .body
                        .unwrap_or_else(|| r#"{"error":"rejected by guard"}"#.to_string());
                    return Ok(crate::runtime::response::text_response(
                        status,
                        "application/json",
                        body,
                    ));
                }
                _ => unreachable!("run_guard only passes allow/deny through"),
            }
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg() -> FunctionConfig {
        FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            protocol: Default::default(),
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
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
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        }
    }

    #[test]
    fn process_handler_implicit_route_uses_function_name() {
        let riz_state = Arc::new(crate::state::RizState::new());
        let pm = Arc::new(ProcessManager::new(riz_state));
        let h = ProcessHandler::for_function("api", &make_cfg(), pm);
        assert_eq!(h.routes().len(), 1);
        assert_eq!(h.routes()[0].path, "/api");
        assert_eq!(h.routes()[0].method, RouteMethod::Any);
        assert_eq!(h.name(), "api");
    }

    #[test]
    fn process_handler_declares_multiple_explicit_routes() {
        let mut cfg = make_cfg();
        cfg.routes = vec![
            RouteSpec {
                path: "/api".into(),
                method: "GET".into(),
            },
            RouteSpec {
                path: "/api/{proxy+}".into(),
                method: "ANY".into(),
            },
        ];
        let riz_state = Arc::new(crate::state::RizState::new());
        let pm = Arc::new(ProcessManager::new(riz_state));
        let h = ProcessHandler::for_function("api", &cfg, pm);
        assert_eq!(h.routes().len(), 2);
        assert_eq!(h.routes()[0].path, "/api");
        assert_eq!(h.routes()[0].method, RouteMethod::Get);
        assert_eq!(h.routes()[1].path, "/api/{proxy+}");
        assert_eq!(h.routes()[1].method, RouteMethod::Any);
    }

    // PoolError → HandlerError → HTTP status mapping tests.
    // Each test exercises one non-Other PoolError variant and asserts the
    // HandlerError variant and status code it maps to via ProcessHandler::invoke.

    fn pool_error_to_handler_error(e: PoolError) -> HandlerError {
        match e {
            PoolError::Timeout(_, ms) => HandlerError::Timeout(ms),
            PoolError::SemaphoreExhausted(_) => HandlerError::Overloaded(0),
            PoolError::SemaphoreClosed(name) => {
                HandlerError::Internal(format!("pool closed: {name}"))
            }
            PoolError::NoPool(name) => {
                HandlerError::Internal(format!("function not configured: {name}"))
            }
            PoolError::InvalidResponse(_, detail) => {
                HandlerError::Process(format!("bad gateway: {detail}"))
            }
            PoolError::Other(_, err) => HandlerError::Process(err.to_string()),
        }
    }

    #[test]
    fn pool_error_timeout_maps_to_504() {
        let err = PoolError::Timeout("api".into(), 5000);
        let handler_err = pool_error_to_handler_error(err);
        assert_eq!(
            handler_err.status_code(),
            504,
            "PoolError::Timeout must map to HTTP 504"
        );
        assert!(matches!(handler_err, HandlerError::Timeout(5000)));
    }

    #[test]
    fn pool_error_semaphore_exhausted_maps_to_429() {
        let err = PoolError::SemaphoreExhausted("api".into());
        let handler_err = pool_error_to_handler_error(err);
        assert_eq!(
            handler_err.status_code(),
            429,
            "PoolError::SemaphoreExhausted must map to HTTP 429"
        );
        assert!(matches!(handler_err, HandlerError::Overloaded(_)));
    }

    #[test]
    fn pool_error_semaphore_closed_maps_to_503() {
        let err = PoolError::SemaphoreClosed("api".into());
        let handler_err = pool_error_to_handler_error(err);
        assert_eq!(
            handler_err.status_code(),
            500,
            "PoolError::SemaphoreClosed maps to HandlerError::Internal (500)"
        );
        assert!(matches!(handler_err, HandlerError::Internal(_)));
        if let HandlerError::Internal(msg) = &handler_err {
            assert!(
                msg.contains("pool closed"),
                "message must mention pool closed"
            );
        }
    }

    #[test]
    fn pool_error_no_pool_maps_to_503_internal() {
        let err = PoolError::NoPool("missing-fn".into());
        let handler_err = pool_error_to_handler_error(err);
        assert_eq!(
            handler_err.status_code(),
            500,
            "PoolError::NoPool maps to HandlerError::Internal (500)"
        );
        assert!(matches!(handler_err, HandlerError::Internal(_)));
        if let HandlerError::Internal(msg) = &handler_err {
            assert!(
                msg.contains("function not configured"),
                "message must say function not configured"
            );
        }
    }

    #[test]
    fn pool_error_invalid_response_maps_to_502() {
        let err = PoolError::InvalidResponse("api".into(), "unexpected token".into());
        let handler_err = pool_error_to_handler_error(err);
        assert_eq!(
            handler_err.status_code(),
            502,
            "PoolError::InvalidResponse must map to HTTP 502"
        );
        assert!(matches!(handler_err, HandlerError::Process(_)));
        if let HandlerError::Process(msg) = &handler_err {
            assert!(msg.contains("bad gateway"), "message must say bad gateway");
        }
    }
}
