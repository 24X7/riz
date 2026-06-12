//! Encoding helpers: path-param substitution, URL encoding, HTTP response
//! builders, and JSON-RPC envelope builders used by the MCP handler.

use crate::gateway::ApiGatewayV2httpResponse;
use crate::runtime::response;
use std::collections::HashMap;

/// Wrap any JSON value in a 200 response with content-type application/json.
pub(super) fn json_response(value: serde_json::Value) -> ApiGatewayV2httpResponse {
    response::json_response(200, &value)
}

/// 202 Accepted — used when the entire request was notifications.
/// Streamable HTTP (MCP 2025-03-26+) mandates 202 for bodies that contain
/// only notifications/responses (nothing for the server to answer).
pub(super) fn accepted_response() -> ApiGatewayV2httpResponse {
    response::empty_response(202)
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

/// MCP `outputSchema` for every Riz tool: the AWS Lambda API Gateway v2
/// response envelope. Returning this on `tools/list` lets MCP 2025-06-18+
/// clients validate `structuredContent` on `tools/call` responses without
/// re-parsing `content[0].text`.
pub(super) fn lambda_response_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "statusCode": {"type": "integer", "minimum": 100, "maximum": 599},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}},
            "cookies": {"type": "array", "items": {"type": "string"}},
            "body": {"type": "string"},
            "isBase64Encoded": {"type": "boolean"}
        },
        "required": ["statusCode"]
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
