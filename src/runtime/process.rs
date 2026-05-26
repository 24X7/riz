//! ProcessHandler — one per FUNCTION (not per route). Holds the function's
//! routes list (Vec<RouteEntry>), the function name, and an Arc<ProcessManager>
//! to delegate invocation to. The same handler is matched by the Router for
//! every route the function declares; one pool serves them all.

use async_trait::async_trait;
use std::sync::Arc;
use crate::config::{FunctionConfig, RouteSpec};
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::process::ProcessManager;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};

pub struct ProcessHandler {
    name: String,
    routes: Vec<RouteEntry>,
    timeout_ms: u64,
    integration_timeout_ms: u64,
    /// Stage variables injected into the event before handler invocation.
    /// Matches AWS API GW v2's `stageVariables` field — per-deployment-stage
    /// config the handler reads at runtime.
    stage_variables: std::collections::HashMap<String, String>,
    process_manager: Arc<ProcessManager>,
}

impl ProcessHandler {
    pub fn for_function(
        name: &str,
        cfg: &FunctionConfig,
        process_manager: Arc<ProcessManager>,
    ) -> Self {
        let routes: Vec<RouteEntry> = cfg.effective_routes(name)
            .into_iter()
            .map(|RouteSpec { path, method }| RouteEntry {
                method: RouteMethod::from_str(&method),
                path,
            })
            .collect();
        Self {
            name: name.to_string(),
            routes,
            timeout_ms: cfg.timeout_ms,
            integration_timeout_ms: cfg.integration_timeout_ms,
            stage_variables: cfg.stage_variables.clone(),
            process_manager,
        }
    }

    pub fn function_name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl LambdaHandler for ProcessHandler {
    fn name(&self) -> &str { &self.name }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, mut event: ApiGatewayV2httpRequest) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        // Inject this function's stage variables before the handler sees the event.
        for (k, v) in &self.stage_variables {
            event.stage_variables.insert(k.clone(), v.clone());
        }

        // Two timeouts here, matching AWS:
        // - integration_timeout_ms: wraps the whole call. If exceeded, we
        //   return 504 to the client without waiting for the handler.
        // - timeout_ms: enforced INSIDE process_manager.invoke; if exceeded,
        //   the child process is killed and respawned.
        let invoke = self.process_manager.invoke(&self.name, &event, self.timeout_ms);
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(self.integration_timeout_ms),
            invoke,
        ).await;

        match outcome {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => {
                let msg = e.to_string();
                if msg.contains("timeout") {
                    Err(HandlerError::Timeout(self.timeout_ms))
                } else if msg.contains("no pool") || msg.contains("semaphore closed") {
                    Err(HandlerError::Internal(msg))
                } else {
                    Err(HandlerError::Process(msg))
                }
            }
            Err(_elapsed) => Err(HandlerError::Timeout(self.integration_timeout_ms)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg() -> FunctionConfig {
        FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 1,
            routes: vec![],
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
            RouteSpec { path: "/api".into(), method: "GET".into() },
            RouteSpec { path: "/api/{proxy+}".into(), method: "ANY".into() },
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
}
