use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::runtime::{error_response, HandlerError, LambdaHandler};
use std::sync::Arc;

pub struct Router {
    handlers: Vec<Arc<dyn LambdaHandler>>,
}

/// Result of a dispatch — exposes the matched FUNCTION NAME so callers can
/// attribute metrics, health, and access logs to the function (mirrors AWS
/// CloudWatch per-function metric semantics), plus the actual response.
pub struct DispatchOutcome {
    pub function_name: String,
    pub response: ApiGatewayV2httpResponse,
}

impl Router {
    pub fn new(handlers: Vec<Arc<dyn LambdaHandler>>) -> Self {
        Self { handlers }
    }

    pub fn empty() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Stable key format used in logs/metrics/registry.
    pub fn route_key(method: &str, pattern: &str) -> String {
        format!("{} {}", method.to_uppercase(), pattern)
    }

    pub fn handlers(&self) -> &[Arc<dyn LambdaHandler>] {
        &self.handlers
    }

    /// Dispatch one event through the first matching handler. Extracts any
    /// path parameters from `:name`-style segments into `event.path_parameters`.
    /// Returns the matched route_key alongside the handler's response (or a
    /// synthetic 404 response if nothing matched).
    pub async fn dispatch(
        &self,
        mut event: ApiGatewayV2httpRequest,
    ) -> Result<DispatchOutcome, HandlerError> {
        // The AWS event's authoritative method/path live in requestContext.http.
        let method = event.request_context.http.method.as_str().to_string();
        let path = event.request_context.http.path.clone().unwrap_or_default();

        for h in &self.handlers {
            for r in h.routes() {
                if let Some(params) = r.match_path(&method, &path) {
                    // Inject path params into event.path_parameters.
                    for (k, v) in params {
                        event.path_parameters.insert(k, v);
                    }
                    let function_name = h.name().to_string();
                    let response = h.invoke(event).await?;
                    return Ok(DispatchOutcome {
                        function_name,
                        response,
                    });
                }
            }
        }
        Ok(DispatchOutcome {
            function_name: "_unmatched".into(),
            response: error_response(404, "not found"),
        })
    }
}

/// Decodes a percent-encoded string (e.g. "foo%2Fbar" → "foo/bar").
/// Kept available for future path-param support (Spec B).
#[allow(dead_code)]
pub fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut bytes = s.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h = bytes.next();
            let l = bytes.next();
            if let (Some(h), Some(l)) = (h, l) {
                if let (Some(hi), Some(lo)) = (hex_val(h), hex_val(l)) {
                    result.push(char::from(hi << 4 | lo));
                    continue;
                }
                result.push('%');
                result.push(char::from(h));
                result.push(char::from(l));
            } else {
                result.push('%');
            }
        } else {
            result.push(char::from(b));
        }
    }
    result
}

#[allow(dead_code)]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{
        ApiGatewayV2httpRequestContext, ApiGatewayV2httpRequestContextHttpDescription, Body,
    };
    use crate::runtime::{LambdaHandler, RouteEntry, RouteMethod};
    use async_trait::async_trait;
    use http::{HeaderMap, Method};

    struct StubHandler {
        name: String,
        routes: Vec<RouteEntry>,
        body: String,
    }

    #[async_trait]
    impl LambdaHandler for StubHandler {
        fn name(&self) -> &str {
            &self.name
        }
        fn routes(&self) -> &[RouteEntry] {
            &self.routes
        }
        async fn invoke(
            &self,
            _event: ApiGatewayV2httpRequest,
        ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
            Ok(ApiGatewayV2httpResponse {
                status_code: 200,
                headers: HeaderMap::new(),
                multi_value_headers: HeaderMap::new(),
                body: Some(Body::Text(self.body.clone())),
                is_base64_encoded: false,
                cookies: Vec::new(),
            })
        }
    }

    pub(crate) fn make_event(method: &str, path: &str) -> ApiGatewayV2httpRequest {
        let ctx = ApiGatewayV2httpRequestContext {
            http: ApiGatewayV2httpRequestContextHttpDescription {
                method: Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET),
                path: Some(path.to_string()),
                protocol: Some("HTTP/1.1".into()),
                source_ip: Some("127.0.0.1".into()),
                user_agent: Some("riz-test".into()),
            },
            request_id: Some("req-1".into()),
            time_epoch: 0,
            ..Default::default()
        };
        ApiGatewayV2httpRequest {
            version: Some("2.0".into()),
            route_key: Some(format!("{method} {path}")),
            raw_path: Some(path.into()),
            raw_query_string: Some(String::new()),
            cookies: None,
            headers: HeaderMap::new(),
            query_string_parameters: Default::default(),
            path_parameters: Default::default(),
            request_context: ctx,
            stage_variables: Default::default(),
            body: None,
            is_base64_encoded: false,
            kind: None,
            method_arn: None,
            http_method: Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET),
            identity_source: None,
            authorization_token: None,
            resource: None,
        }
    }

    #[test]
    fn route_key_format_preserved() {
        assert_eq!(Router::route_key("get", "/api"), "GET /api");
    }

    #[tokio::test]
    async fn first_matching_handler_wins() {
        let h1 = Arc::new(StubHandler {
            name: "first".into(),
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/api".into(),
            }],
            body: "from-first".into(),
        });
        let h2 = Arc::new(StubHandler {
            name: "second".into(),
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/api".into(),
            }],
            body: "from-second".into(),
        });
        let router = Router::new(vec![h1, h2]);
        let outcome = router.dispatch(make_event("GET", "/api")).await.unwrap();
        match outcome.response.body.expect("body should be set") {
            Body::Text(s) => assert_eq!(s, "from-first"),
            other => panic!("expected Text body, got {other:?}"),
        }
        assert_eq!(outcome.function_name, "first");
    }

    #[tokio::test]
    async fn no_match_returns_404() {
        let router = Router::empty();
        let outcome = router
            .dispatch(make_event("GET", "/no-such"))
            .await
            .unwrap();
        assert_eq!(outcome.response.status_code, 404);
    }

    #[tokio::test]
    async fn method_mismatch_returns_404() {
        let h = Arc::new(StubHandler {
            name: "only-get".into(),
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/api".into(),
            }],
            body: "x".into(),
        });
        let router = Router::new(vec![h]);
        let outcome = router.dispatch(make_event("POST", "/api")).await.unwrap();
        assert_eq!(outcome.response.status_code, 404);
    }

    #[tokio::test]
    async fn route_method_any_matches_all_methods() {
        let h = Arc::new(StubHandler {
            name: "any".into(),
            routes: vec![RouteEntry {
                method: RouteMethod::Any,
                path: "/api".into(),
            }],
            body: "ok".into(),
        });
        let router = Router::new(vec![h]);
        for m in &["GET", "POST", "PUT", "DELETE", "PATCH"] {
            let outcome = router.dispatch(make_event(m, "/api")).await.unwrap();
            assert_eq!(outcome.response.status_code, 200, "method {m} should match");
        }
    }

    struct CapturingHandler {
        routes: Vec<RouteEntry>,
        captured: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    #[async_trait::async_trait]
    impl LambdaHandler for CapturingHandler {
        fn name(&self) -> &str {
            "capturing"
        }
        fn routes(&self) -> &[RouteEntry] {
            &self.routes
        }
        async fn invoke(
            &self,
            event: ApiGatewayV2httpRequest,
        ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
            *self.captured.lock().unwrap() = event.path_parameters.clone();
            Ok(ApiGatewayV2httpResponse {
                status_code: 200,
                headers: HeaderMap::new(),
                multi_value_headers: HeaderMap::new(),
                body: None,
                is_base64_encoded: false,
                cookies: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn dispatch_injects_path_params_into_event() {
        let h = Arc::new(CapturingHandler {
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/accounts/{id}".into(),
            }],
            captured: std::sync::Mutex::new(Default::default()),
        });
        let router = Router::new(vec![h.clone()]);
        let outcome = router
            .dispatch(make_event("GET", "/accounts/42"))
            .await
            .unwrap();
        assert_eq!(outcome.response.status_code, 200);
        assert_eq!(outcome.function_name, "capturing");
        let captured = h.captured.lock().unwrap();
        assert_eq!(captured.get("id").map(String::as_str), Some("42"));
    }

    #[tokio::test]
    async fn dispatch_attributes_to_function_name_not_route() {
        let h = Arc::new(CapturingHandler {
            routes: vec![RouteEntry {
                method: RouteMethod::Get,
                path: "/orgs/{org}/repos/{repo}".into(),
            }],
            captured: std::sync::Mutex::new(Default::default()),
        });
        let router = Router::new(vec![h]);
        let outcome = router
            .dispatch(make_event("GET", "/orgs/anthropic/repos/riz"))
            .await
            .unwrap();
        assert_eq!(outcome.function_name, "capturing",
            "metrics must attribute to the function name, mirroring AWS CloudWatch per-function aggregation");
    }

    #[tokio::test]
    async fn router_matches_aws_path_syntax() {
        let h = Arc::new(CapturingHandler {
            routes: vec![RouteEntry {
                method: RouteMethod::Any,
                path: "/api/{proxy+}".into(),
            }],
            captured: std::sync::Mutex::new(Default::default()),
        });
        let router = Router::new(vec![h.clone()]);
        let outcome = router
            .dispatch(make_event("GET", "/api/users/42/profile"))
            .await
            .unwrap();
        assert_eq!(outcome.response.status_code, 200);
        assert_eq!(outcome.function_name, "capturing");
        let captured = h.captured.lock().unwrap();
        assert_eq!(
            captured.get("proxy").map(String::as_str),
            Some("users/42/profile")
        );
    }

    #[test]
    fn percent_decode_helper_still_works() {
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("normal"), "normal");
    }
}
