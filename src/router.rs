use std::sync::Arc;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::runtime::{HandlerError, LambdaHandler};

pub struct Router {
    handlers: Vec<Arc<dyn LambdaHandler>>,
}

impl Router {
    pub fn new(handlers: Vec<Arc<dyn LambdaHandler>>) -> Self {
        Self { handlers }
    }

    pub fn empty() -> Self {
        Self { handlers: Vec::new() }
    }

    /// Stable key format used in logs/metrics/registry.
    pub fn route_key(method: &str, pattern: &str) -> String {
        format!("{} {}", method.to_uppercase(), pattern)
    }

    pub fn handlers(&self) -> &[Arc<dyn LambdaHandler>] {
        &self.handlers
    }

    /// Dispatch one event through the first matching handler.
    /// Returns Ok(404 response) if no handler claims the route.
    pub async fn dispatch(
        &self,
        event: GatewayRequest,
    ) -> Result<GatewayResponse, HandlerError> {
        let method = event.request_context.http.method.clone();
        let path = event.request_context.http.path.clone();
        for h in &self.handlers {
            for r in h.routes() {
                if r.matches(&method, &path) {
                    return h.invoke(event).await;
                }
            }
        }
        Ok(GatewayResponse::error(404, "not found"))
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
    use crate::gateway::{HttpContext, RequestContext};
    use crate::runtime::{LambdaHandler, RouteEntry, RouteMethod};
    use async_trait::async_trait;
    use std::collections::HashMap;

    struct StubHandler {
        name: String,
        routes: Vec<RouteEntry>,
        body: String,
    }

    #[async_trait]
    impl LambdaHandler for StubHandler {
        fn name(&self) -> &str { &self.name }
        fn routes(&self) -> &[RouteEntry] { &self.routes }
        async fn invoke(&self, _event: GatewayRequest) -> Result<GatewayResponse, HandlerError> {
            Ok(GatewayResponse {
                status_code: 200,
                headers: None,
                body: Some(self.body.clone()),
                is_base64_encoded: None,
            })
        }
    }

    fn make_event(method: &str, path: &str) -> GatewayRequest {
        GatewayRequest {
            version: "2.0".into(),
            route_key: format!("{method} {path}"),
            raw_path: path.into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: method.into(),
                    path: path.into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "req-1".into(),
                time_epoch: 0,
            },
            path_parameters: None,
            body: None,
            is_base64_encoded: false,
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
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "from-first".into(),
        });
        let h2 = Arc::new(StubHandler {
            name: "second".into(),
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "from-second".into(),
        });
        let router = Router::new(vec![h1, h2]);
        let resp = router.dispatch(make_event("GET", "/api")).await.unwrap();
        assert_eq!(resp.body.as_deref(), Some("from-first"));
    }

    #[tokio::test]
    async fn no_match_returns_404() {
        let router = Router::empty();
        let resp = router.dispatch(make_event("GET", "/no-such")).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn method_mismatch_returns_404() {
        let h = Arc::new(StubHandler {
            name: "only-get".into(),
            routes: vec![RouteEntry { method: RouteMethod::Get, path: "/api".into() }],
            body: "x".into(),
        });
        let router = Router::new(vec![h]);
        let resp = router.dispatch(make_event("POST", "/api")).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn route_method_any_matches_all_methods() {
        let h = Arc::new(StubHandler {
            name: "any".into(),
            routes: vec![RouteEntry { method: RouteMethod::Any, path: "/api".into() }],
            body: "ok".into(),
        });
        let router = Router::new(vec![h]);
        for m in &["GET", "POST", "PUT", "DELETE", "PATCH"] {
            let resp = router.dispatch(make_event(m, "/api")).await.unwrap();
            assert_eq!(resp.status_code, 200, "method {m} should match");
        }
    }

    #[test]
    fn percent_decode_helper_still_works() {
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("normal"), "normal");
    }
}
