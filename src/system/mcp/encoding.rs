//! Encoding helpers: path-param substitution, URL encoding, HTTP response
//! builders, and JSON-RPC envelope builders used by the MCP handler.

use crate::gateway::{ApiGatewayV2httpResponse, Body};
use http::{header, HeaderMap, HeaderValue};
use std::collections::HashMap;

/// Wrap any JSON value in a 200 response with content-type application/json.
pub(super) fn json_response(value: serde_json::Value) -> ApiGatewayV2httpResponse {
    let json = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    ApiGatewayV2httpResponse {
        status_code: 200,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(json)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// 204 No Content — used when the entire request was notifications.
pub(super) fn no_content_response() -> ApiGatewayV2httpResponse {
    ApiGatewayV2httpResponse {
        status_code: 204,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        body: None,
        is_base64_encoded: false,
        cookies: Vec::new(),
    }
}

/// Build a JSON-RPC error envelope around a single id, return as a full HTTP
/// response. Used at the top of `invoke` for parse/batch-shape failures.
pub(super) fn jsonrpc_error_response(
    id: serde_json::Value,
    code: i32,
    message: &str,
) -> ApiGatewayV2httpResponse {
    json_response(jsonrpc_error_value(id, code, message))
}

/// Just the JSON-RPC error envelope as a JSON value — used inside batch
/// processing where we collect responses into an array.
pub(super) fn jsonrpc_error_value(
    id: serde_json::Value,
    code: i32,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

/// Schema describing the generic envelope every MCP tool accepts.
pub(super) fn generic_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "route": {"type": "string", "description": "Optional \"METHOD /path\" selector when the function declares multiple routes. Omit to use the first declared route."},
            "body": {"type": "string", "description": "Request body. Set isBase64Encoded:true for binary."},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}},
            "queryParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "pathParams": {"type": "object", "additionalProperties": {"type": "string"}},
            "isBase64Encoded": {"type": "boolean", "default": false}
        }
    })
}

/// Substitute `{name}` and `{name+}` segments in `pattern` with values from
/// `params`. Segments without a matching param key are left as-is (caller
/// error — the Router's match will then reject the request as a 404).
pub(super) fn substitute_path_params(pattern: &str, params: &HashMap<String, String>) -> String {
    if !pattern.contains('{') {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len());
    let mut first = true;
    for seg in pattern.trim_start_matches('/').split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        // `{name+}` greedy
        if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix("+}")) {
            if let Some(v) = params.get(inner) {
                out.push_str(v);
            } else {
                out.push_str(seg);
            }
            continue;
        }
        // `{name}` single
        if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if let Some(v) = params.get(inner) {
                out.push_str(v);
            } else {
                out.push_str(seg);
            }
        } else {
            out.push_str(seg);
        }
    }
    if pattern.starts_with('/') {
        let mut prefixed = String::with_capacity(out.len() + 1);
        prefixed.push('/');
        prefixed.push_str(&out);
        prefixed
    } else {
        out
    }
}

pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}
