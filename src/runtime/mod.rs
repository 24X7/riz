//! Riz runtime — LambdaHandler trait and the canonical request/response types.
//! All handlers (user functions and system functions) implement LambdaHandler.

pub mod process;

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use crate::gateway::{GatewayRequest, GatewayResponse};

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
    async fn invoke(&self, event: GatewayRequest) -> Result<GatewayResponse, HandlerError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: RouteMethod,
    pub path: String,
}

impl RouteEntry {
    pub fn matches(&self, method: &str, path: &str) -> bool {
        self.method.matches(method) && self.path == path
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

    pub fn to_response(&self) -> GatewayResponse {
        #[derive(Serialize)]
        struct Body<'a> { message: &'a str }
        let body = serde_json::to_string(&Body { message: &self.to_string() })
            .unwrap_or_else(|_| String::from(r#"{"message":"internal"}"#));
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        GatewayResponse {
            status_code: self.status_code(),
            headers: Some(headers),
            body: Some(body),
            is_base64_encoded: None,
        }
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
        let body = resp.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["message"].as_str().unwrap().contains("timeout"));
    }
}
