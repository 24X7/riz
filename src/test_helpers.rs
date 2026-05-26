//! Shared test fixtures.
//!
//! Builds a properly-shaped AWS API Gateway v2 HTTP event for unit tests
//! across the crate. Production code should never construct events directly —
//! they come from server.rs (built from incoming axum requests).

use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
};
use http::{HeaderMap, Method};

pub fn make_event(method: &str, path: &str) -> ApiGatewayV2httpRequest {
    let m = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
    let ctx = ApiGatewayV2httpRequestContext {
        http: ApiGatewayV2httpRequestContextHttpDescription {
            method: m.clone(),
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
        http_method: m,
        identity_source: None,
        authorization_token: None,
        resource: None,
    }
}

pub fn make_event_with_body(method: &str, path: &str, body: &str) -> ApiGatewayV2httpRequest {
    let mut e = make_event(method, path);
    e.body = Some(body.to_string());
    e
}
