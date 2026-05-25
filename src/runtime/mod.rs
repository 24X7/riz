//! Riz runtime — LambdaHandler trait and the canonical request/response types.
//! All handlers (user functions and system functions) implement LambdaHandler.

pub mod process;

use async_trait::async_trait;
use http::HeaderMap;
use serde::Serialize;
use std::collections::HashMap;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse, Body};

#[async_trait]
pub trait LambdaHandler: Send + Sync {
    /// Stable name for logs and registry display.
    fn name(&self) -> &str;

    /// Routes this handler serves. Each is checked against the incoming
    /// request; the router picks the first handler whose RouteEntry matches.
    fn routes(&self) -> &[RouteEntry];

    /// Optional: synchronous shutdown hook (e.g. kill child processes).
    /// Default: no-op.
    fn on_shutdown(&self) {}

    /// Process one event. Returns Ok(response) on success, Err for runtime
    /// failures (which the router converts to a 4xx/5xx response).
    async fn invoke(&self, event: ApiGatewayV2httpRequest) -> Result<ApiGatewayV2httpResponse, HandlerError>;
}

/// Canonical error-shape response builder. Replaces the old
/// `GatewayResponse::error` constructor. Body is a JSON `{"message": "..."}`
/// so it round-trips through the AWS Body::Text encoding.
pub fn error_response(status_code: u16, message: &str) -> ApiGatewayV2httpResponse {
    #[derive(Serialize)]
    struct E<'a> { message: &'a str }
    let body = serde_json::to_string(&E { message })
        .unwrap_or_else(|_| String::from(r#"{"message":"internal"}"#));
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    ApiGatewayV2httpResponse {
        status_code: status_code as i64,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(body)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: RouteMethod,
    pub path: String,
}

impl RouteEntry {
    /// Returns Some(params) if the entry matches the given request — the map
    /// contains any path parameters extracted from `{name}` or `{name+}`
    /// segments (empty when the pattern has no params). Returns None on
    /// mismatch.
    ///
    /// Pattern syntax matches AWS API Gateway v2:
    /// - `/users` — exact match
    /// - `/users/{id}` — single-segment capture into `id`
    /// - `/files/{proxy+}` — greedy capture: matches the rest of the path
    ///   (one or more segments), joined with `/` and stored in `proxy`
    pub fn match_path(&self, method: &str, path: &str) -> Option<HashMap<String, String>> {
        if !self.method.matches(method) {
            return None;
        }
        // Fast path: no `{` → exact compare.
        if !self.path.contains('{') {
            if self.path == path {
                return Some(HashMap::new());
            }
            return None;
        }
        let pattern_parts: Vec<&str> = self.path.trim_matches('/').split('/').collect();
        let path_parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let mut params = HashMap::new();
        for (idx, pat) in pattern_parts.iter().enumerate() {
            // Greedy `{name+}` — consumes all remaining path segments.
            if let Some(inner) = pat.strip_prefix('{').and_then(|s| s.strip_suffix("+}")) {
                if idx >= path_parts.len() {
                    return None;
                }
                let tail = path_parts[idx..].join("/");
                params.insert(inner.to_string(), crate::router::percent_decode(&tail));
                return Some(params);
            }
            // Single-segment `{name}` capture.
            if let Some(inner) = pat.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if idx >= path_parts.len() {
                    return None;
                }
                params.insert(inner.to_string(), crate::router::percent_decode(path_parts[idx]));
                continue;
            }
            // Literal segment.
            if idx >= path_parts.len() || *pat != path_parts[idx] {
                return None;
            }
        }
        // No greedy capture consumed the rest — segment counts must match.
        if pattern_parts.len() != path_parts.len() {
            return None;
        }
        Some(params)
    }

    /// Convenience boolean form for tests / callers that don't need the params.
    pub fn matches(&self, method: &str, path: &str) -> bool {
        self.match_path(method, path).is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMethod {
    Any,
    Get, Post, Put, Delete, Patch, Head, Options,
}

impl RouteMethod {
    pub fn matches(&self, method: &str) -> bool {
        match self {
            RouteMethod::Any => true,
            other => method.eq_ignore_ascii_case(other.as_str()),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RouteMethod::Any => "ANY",
            RouteMethod::Get => "GET",
            RouteMethod::Post => "POST",
            RouteMethod::Put => "PUT",
            RouteMethod::Delete => "DELETE",
            RouteMethod::Patch => "PATCH",
            RouteMethod::Head => "HEAD",
            RouteMethod::Options => "OPTIONS",
        }
    }

    /// Permissive parse — unknown verbs become Any. The verb usually comes
    /// from `riz.toml` so we never want to fail loading on a typo.
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "GET" => RouteMethod::Get,
            "POST" => RouteMethod::Post,
            "PUT" => RouteMethod::Put,
            "DELETE" => RouteMethod::Delete,
            "PATCH" => RouteMethod::Patch,
            "HEAD" => RouteMethod::Head,
            "OPTIONS" => RouteMethod::Options,
            _ => RouteMethod::Any,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("timeout after {0}ms")]
    Timeout(u64),
    #[error("overloaded (max_concurrent={0})")]
    Overloaded(usize),
    #[error("process error: {0}")]
    Process(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl HandlerError {
    pub fn status_code(&self) -> u16 {
        match self {
            HandlerError::Timeout(_) => 504,
            HandlerError::Overloaded(_) => 429,
            HandlerError::Process(_) => 502,
            HandlerError::InvalidResponse(_) => 500,
            HandlerError::Internal(_) => 500,
        }
    }

    pub fn to_response(&self) -> ApiGatewayV2httpResponse {
        error_response(self.status_code(), &self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_method_matches_any() {
        assert!(RouteMethod::Any.matches("GET"));
        assert!(RouteMethod::Any.matches("POST"));
        assert!(RouteMethod::Any.matches("PUT"));
    }

    #[test]
    fn route_method_matches_specific() {
        assert!(RouteMethod::Get.matches("GET"));
        assert!(RouteMethod::Get.matches("get"));
        assert!(!RouteMethod::Get.matches("POST"));
        assert!(RouteMethod::Post.matches("POST"));
    }

    #[test]
    fn route_method_from_str_parses_common_verbs() {
        assert_eq!(RouteMethod::from_str("GET"), RouteMethod::Get);
        assert_eq!(RouteMethod::from_str("get"), RouteMethod::Get);
        assert_eq!(RouteMethod::from_str("PATCH"), RouteMethod::Patch);
        assert_eq!(RouteMethod::from_str("ANY"), RouteMethod::Any);
        assert_eq!(RouteMethod::from_str("UNKNOWN"), RouteMethod::Any);
    }

    #[test]
    fn route_entry_matches_exact_path() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/api".into() };
        assert!(e.matches("GET", "/api"));
        assert!(!e.matches("POST", "/api"));
        assert!(!e.matches("GET", "/api/users"));
    }

    #[test]
    fn route_entry_match_path_returns_empty_params_for_exact() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/api".into() };
        let params = e.match_path("GET", "/api").unwrap();
        assert!(params.is_empty());
    }

    #[test]
    fn route_entry_extracts_single_path_param() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/accounts/{id}".into() };
        let params = e.match_path("GET", "/accounts/42").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("42"));
    }

    #[test]
    fn route_entry_extracts_multiple_path_params() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/orgs/{org}/repos/{repo}".into() };
        let params = e.match_path("GET", "/orgs/anthropic/repos/riz").unwrap();
        assert_eq!(params.get("org").map(String::as_str), Some("anthropic"));
        assert_eq!(params.get("repo").map(String::as_str), Some("riz"));
    }

    #[test]
    fn route_entry_pattern_rejects_segment_count_mismatch() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/accounts/{id}".into() };
        assert!(e.match_path("GET", "/accounts").is_none());
        assert!(e.match_path("GET", "/accounts/42/profile").is_none());
    }

    #[test]
    fn route_entry_pattern_rejects_method_mismatch() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/accounts/{id}".into() };
        assert!(e.match_path("POST", "/accounts/42").is_none());
    }

    #[test]
    fn route_entry_pattern_percent_decodes_params() {
        let e = RouteEntry { method: RouteMethod::Get, path: "/files/{name}".into() };
        let params = e.match_path("GET", "/files/hello%20world").unwrap();
        assert_eq!(params.get("name").map(String::as_str), Some("hello world"));
    }

    #[test]
    fn route_entry_greedy_proxy_captures_rest_of_path() {
        let e = RouteEntry { method: RouteMethod::Any, path: "/api/{proxy+}".into() };
        let p = e.match_path("GET", "/api/users/42/profile").unwrap();
        assert_eq!(p.get("proxy").map(String::as_str), Some("users/42/profile"));
    }

    #[test]
    fn route_entry_greedy_requires_at_least_one_segment() {
        let e = RouteEntry { method: RouteMethod::Any, path: "/api/{proxy+}".into() };
        // /api alone does not match /api/{proxy+}
        assert!(e.match_path("GET", "/api").is_none());
    }

    #[test]
    fn route_entry_greedy_extracts_when_only_one_segment_follows() {
        let e = RouteEntry { method: RouteMethod::Any, path: "/api/{proxy+}".into() };
        let p = e.match_path("GET", "/api/users").unwrap();
        assert_eq!(p.get("proxy").map(String::as_str), Some("users"));
    }

    #[test]
    fn handler_error_status_codes() {
        assert_eq!(HandlerError::Timeout(30).status_code(), 504);
        assert_eq!(HandlerError::Overloaded(10).status_code(), 429);
        assert_eq!(HandlerError::Process("died".into()).status_code(), 502);
        assert_eq!(HandlerError::InvalidResponse("bad json".into()).status_code(), 500);
        assert_eq!(HandlerError::Internal("x".into()).status_code(), 500);
    }

    #[test]
    fn handler_error_to_response_has_json_body() {
        let err = HandlerError::Timeout(30);
        let resp = err.to_response();
        assert_eq!(resp.status_code, 504);
        let body_text = match resp.body.expect("body should be set") {
            Body::Text(s) => s,
            other => panic!("expected text body, got {other:?}"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&body_text).unwrap();
        assert!(parsed["message"].as_str().unwrap().contains("timeout"));
    }
}
