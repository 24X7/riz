//! /_riz/registry handler — JSON manifest of all mounted routes (user + system).

use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use http::{header, HeaderMap, HeaderValue};
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse, Body};
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
    /// Function name (e.g. "api", "users") — matches AWS Lambda function naming.
    name: String,
    /// All routes this function serves, as "METHOD /path" strings.
    routes: Vec<String>,
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

    async fn invoke(&self, _event: ApiGatewayV2httpRequest) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let functions = self.riz_state.functions.read().await;
        let mut out: Vec<RegistryFunction> = Vec::with_capacity(functions.len());
        for (_, f) in functions.iter() {
            let (runtime, handler, timeout_ms, concurrency, cache_ttl_secs) = match &f.config {
                Some(c) => (
                    Some(c.runtime.as_str().to_string()),
                    Some(c.handler.to_string_lossy().to_string()),
                    Some(c.timeout_ms),
                    Some(c.concurrency),
                    c.cache_ttl_secs,
                ),
                None => (None, None, None, None, None),
            };
            let kind = match f.kind {
                FunctionKind::User => "user",
                FunctionKind::System => "system",
            };
            out.push(RegistryFunction {
                name: f.name.clone(),
                routes: f.routes.clone(),
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
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(ApiGatewayV2httpResponse {
            status_code: 200,
            headers,
            multi_value_headers: HeaderMap::new(),
            body: Some(Body::Text(json)),
            is_base64_encoded: false,
            cookies: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FunctionState;
    use crate::test_helpers::make_event;

    fn evt() -> ApiGatewayV2httpRequest { make_event("GET", "/_riz/registry") }

    fn body_text(resp: &ApiGatewayV2httpResponse) -> String {
        match resp.body.as_ref().expect("body") {
            Body::Text(s) => s.clone(),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    fn user_state() -> FunctionState {
        let c = crate::config::FunctionConfig {
            runtime: crate::config::RuntimeKind::Bun,
            handler: std::path::PathBuf::from("./api.ts"),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency: 3,
            routes: vec![],
        };
        FunctionState::user("api", c)
    }

    #[tokio::test]
    async fn registry_returns_json_with_version() {
        let s = Arc::new(RizState::new());
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        assert!(body["version"].is_string());
        assert!(body["functions"].is_array());
    }

    #[tokio::test]
    async fn registry_lists_user_functions_with_full_fields() {
        let s = Arc::new(RizState::new());
        s.register(user_state()).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "user");
        assert_eq!(f["name"], "api");
        // Routes is an array of "METHOD /path" strings.
        let routes = f["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0], "ANY /api");
        assert_eq!(f["runtime"], "bun");
        assert_eq!(f["timeout_ms"], 5000);
        assert_eq!(f["concurrency"], 3);
    }

    #[tokio::test]
    async fn registry_lists_system_functions_with_nulls() {
        let s = Arc::new(RizState::new());
        s.register(FunctionState::system("_riz_health", vec!["GET /_riz/health".into()])).await;
        let h = RegistryHandler::new(s);
        let resp = h.invoke(evt()).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text(&resp)).unwrap();
        let f = &body["functions"][0];
        assert_eq!(f["kind"], "system");
        assert_eq!(f["name"], "_riz_health");
        let routes = f["routes"].as_array().unwrap();
        assert_eq!(routes[0], "GET /_riz/health");
        assert!(f["runtime"].is_null());
        assert!(f["handler"].is_null());
    }
}
