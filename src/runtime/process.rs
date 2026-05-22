//! ProcessHandler — owns a (route_key, timeout) pair and delegates invocation
//! to a shared `ProcessManager`. This keeps the well-tested pool machinery
//! intact while exposing each route as a `LambdaHandler` for the new router.

use async_trait::async_trait;
use std::sync::Arc;
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::ProcessManager;
use crate::router::Router;
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};

pub struct ProcessHandler {
    name: String,
    routes: Vec<RouteEntry>,
    route_key: String,
    timeout_ms: u64,
    process_manager: Arc<ProcessManager>,
}

impl ProcessHandler {
    pub fn for_route(route: &RouteConfig, process_manager: Arc<ProcessManager>) -> Self {
        let route_key = Router::route_key(&route.method, &route.path);
        let method = RouteMethod::from_str(&route.method);
        Self {
            name: route_key.clone(),
            routes: vec![RouteEntry { method, path: route.path.clone() }],
            route_key,
            timeout_ms: route.timeout_ms,
            process_manager,
        }
    }

    pub fn route_key(&self) -> &str {
        &self.route_key
    }
}

#[async_trait]
impl LambdaHandler for ProcessHandler {
    fn name(&self) -> &str { &self.name }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        self.process_manager
            .invoke(&self.route_key, &event, self.timeout_ms)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("timeout") {
                    HandlerError::Timeout(self.timeout_ms)
                } else if msg.contains("no pool") || msg.contains("semaphore closed") {
                    HandlerError::Internal(msg)
                } else {
                    HandlerError::Process(msg)
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_route() -> RouteConfig {
        RouteConfig {
            path: "/api".into(),
            method: "GET".into(),
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./does-not-exist.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        }
    }

    #[test]
    fn process_handler_exposes_route_entry_from_config() {
        let pm = Arc::new(ProcessManager::new());
        let h = ProcessHandler::for_route(&make_route(), pm);
        assert_eq!(h.routes().len(), 1);
        assert_eq!(h.routes()[0].path, "/api");
        assert_eq!(h.routes()[0].method, RouteMethod::Get);
        assert_eq!(h.name(), "GET /api");
        assert_eq!(h.route_key(), "GET /api");
    }
}
