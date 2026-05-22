//! /_riz/registry handler — JSON manifest of all mounted routes (user + system).

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::state::{FunctionKind, RizState};

pub struct RegistryHandler {
    routes: Vec<RouteEntry>,
    riz_state: Arc<RizState>,
}

impl RegistryHandler {
    pub fn new(riz_state: Arc<RizState>) -> Self {
        Self {
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/_riz/registry".into() }],
            riz_state,
        }
    }
}

#[derive(Serialize)]
struct RegistryBody {
    version: &'static str,
    functions: Vec<RegistryFunction>,
}

#[derive(Serialize)]
struct RegistryFunction {
    route_key: String,
    method: String,
    path: String,
    runtime: Option<String>,
    kind: &'static str,
    handler: Option<String>,
    timeout_ms: Option<u64>,
    concurrency: Option<usize>,
    cache_ttl_secs: Option<u64>,
}

#[async_trait]
impl LambdaHandler for RegistryHandler {
    fn name(&self) -> &str { "GET /_riz/registry" }
    fn routes(&self) -> &[RouteEntry] { &self.routes }

    async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<RegistryFunction> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            let (runtime, handler, timeout_ms, concurrency, cache_ttl_secs) = match &f.route {
                Some(r) => (
                    Some(r.runtime.as_str().to_string()),
                    Some(r.handler.to_string_lossy().to_string()),
                    Some(r.timeout_ms),
                    Some(r.concurrency),
                    r.cache_ttl_secs,
                ),
                None => (None, None, None, None, None),
            };
            let kind = match f.kind {
                FunctionKind::User => "user",
                FunctionKind::System => "system",
            };
            let (method, path) = match f.route_key.split_once(' ') {
                Some((m, p)) => (m.to_string(), p.to_string()),
                None => (String::new(), f.route_key.clone()),
            };
            out.push(RegistryFunction {
                route_key: f.route_key.clone(),
                method,
                path,
                runtime,
                kind,
                handler,
                timeout_ms,
                concurrency,
                cache_ttl_secs,
            });
        }
        let body = RegistryBody { version: self.riz_state.version, functions: out };
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
            route_key: "GET /_riz/registry".into(),
            raw_path: "/_riz/registry".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/_riz/registry".into(),
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
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 3,
        };
        FunctionState::user("GET /api", r)
    }

    #[tokio::test]
    async fn registry_returns_json_with_version() {
        let s = Arc::new(RizState::new());
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        assert!(body["version"].is_string());
        assert!(body["functions"].is_array());
    }

    #[tokio::test]
    async fn registry_lists_user_functions_with_full_fields() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "user");
        assert_eq!(f["method"], "GET");
        assert_eq!(f["path"], "/api");
        assert_eq!(f["runtime"], "bun");
        assert_eq!(f["timeout_ms"], 5000);
        assert_eq!(f["concurrency"], 3);
    }

    #[tokio::test]
    async fn registry_lists_system_functions_with_nulls() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("GET /_riz/health")).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_deref().unwrap()).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "system");
        assert_eq!(f["method"], "GET");
        assert_eq!(f["path"], "/_riz/health");
        assert!(f["runtime"].is_null());
        assert!(f["handler"].is_null());
    }
}
