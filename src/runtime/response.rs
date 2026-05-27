//! Response constructors for handlers. Centralises the AWS API GW v2
//! response shape so individual handlers don't repeat 6-field literals.
//!
//! HTTP API v2 does not use multiValueHeaders (multi-Set-Cookie uses the
//! `cookies` array). Builders always emit it empty.

use crate::gateway::{ApiGatewayV2httpResponse, Body};
use http::{header, HeaderMap, HeaderValue};
use serde::Serialize;

/// Build a JSON response with the given status code. The value is serialised
/// with `serde_json::to_string`; on serialisation failure returns a 500
/// with a generic body so callers don't need to handle the error.
pub fn json_response<T: Serialize>(status: u16, value: &T) -> ApiGatewayV2httpResponse {
    let (status, body) = match serde_json::to_string(value) {
        Ok(s) => (status, s),
        Err(e) => {
            tracing::error!(error = %e, "response serialisation failed");
            (
                500,
                r#"{"error":"response serialization failed"}"#.to_string(),
            )
        }
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    ApiGatewayV2httpResponse {
        status_code: status as i64,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(body)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// Build a text response with explicit content-type.
pub fn text_response(status: u16, content_type: &str, body: String) -> ApiGatewayV2httpResponse {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    ApiGatewayV2httpResponse {
        status_code: status as i64,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(body)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// Build an empty-body response for status codes that don't carry payloads
/// (204 No Content, 304 Not Modified, etc.).
pub fn empty_response(status: u16) -> ApiGatewayV2httpResponse {
    ApiGatewayV2httpResponse {
        status_code: status as i64,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        body: None,
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_response_sets_content_type_and_status() {
        let r = json_response(200, &serde_json::json!({"ok": true}));
        assert_eq!(r.status_code, 200);
        assert_eq!(
            r.headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert!(r.body.is_some());
    }

    #[test]
    fn text_response_with_custom_content_type() {
        let r = text_response(201, "text/plain", "hello".into());
        assert_eq!(r.status_code, 201);
        assert_eq!(r.headers.get(header::CONTENT_TYPE).unwrap(), "text/plain");
    }

    #[test]
    fn empty_response_has_no_body() {
        let r = empty_response(204);
        assert_eq!(r.status_code, 204);
        assert!(r.body.is_none());
        assert!(r.headers.is_empty());
    }

    #[test]
    fn json_response_on_serialize_failure_returns_500() {
        // A type whose Serialize impl always errors.
        struct AlwaysFails;
        impl serde::Serialize for AlwaysFails {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let r = json_response(200, &AlwaysFails);
        assert_eq!(r.status_code, 500);
    }
}
