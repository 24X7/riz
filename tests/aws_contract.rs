//! Round-trip every canonical AWS event fixture through the
//! `aws_lambda_events` types we re-export. Any divergence between the
//! AWS-docs shape and our parsed shape fails CI.

use riz::gateway::{ApiGatewayV2httpRequest, ApiGatewayWebsocketProxyRequest};
use serde_json::Value;

/// Normalise a JSON value before equality comparison so that intentional
/// serialisation differences don't fail the round-trip. Every rule here
/// is documented. If you add a rule, add a `// Documented exclusion:` comment
/// explaining WHY — that comment is the audit trail.
fn deep_normalize(mut v: Value) -> Value {
    if let Value::Object(ref mut map) = v {
        // Documented exclusion: aws_lambda_events Option<T> fields that lack
        // `#[serde(skip_serializing_if = "Option::is_none")]` always serialise
        // as JSON `null` even when not present in the original payload. AWS
        // docs examples omit these keys entirely. Stripping nulls from both
        // sides makes the comparison meaningful without hiding real bugs —
        // a field that has an actual value will still diverge if it is wrong.
        map.retain(|_k, v| !v.is_null());

        // Documented exclusion: `ApiGatewayV2httpRequest.http_method` and
        // `ApiGatewayWebsocketProxyRequest.http_method` are inherited from the
        // v1 REST API shape and default to GET via `default_http_method`. The
        // AWS payload format v2 docs do NOT include a top-level `httpMethod`
        // field — it is an artifact of the crate supporting both v1 and v2
        // shapes in one struct. Serialised output always emits `"httpMethod"`
        // with the default value; AWS-docs fixtures do not include it.
        map.remove("httpMethod");

        for (_k, val) in map.iter_mut() {
            *val = deep_normalize(val.clone());
        }
    } else if let Value::Array(ref mut arr) = v {
        for item in arr.iter_mut() {
            *item = deep_normalize(item.clone());
        }
    }
    v
}

#[test]
fn fixture_apigw_v2_http_simple_get_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_simple_get.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.version.as_deref(), Some("2.0"));
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_http_post_with_body_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_post_with_body.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert!(parsed.is_base64_encoded);
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_http_put_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_put.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.http.method.as_str(), "PUT");
    assert_eq!(
        parsed.path_parameters.get("id").map(String::as_str),
        Some("42")
    );
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_http_delete_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_delete.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.http.method.as_str(), "DELETE");
    assert_eq!(
        parsed.path_parameters.get("id").map(String::as_str),
        Some("42")
    );
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_http_patch_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_patch.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.http.method.as_str(), "PATCH");
    assert!(parsed
        .body
        .as_deref()
        .unwrap_or("")
        .contains("\"op\":\"replace\""));
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_connect_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_connect.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(
        parsed.request_context.event_type.as_deref(),
        Some("CONNECT")
    );
    assert_eq!(
        parsed.request_context.route_key.as_deref(),
        Some("$connect")
    );
    assert!(parsed.request_context.connection_id.is_some());
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_message_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_message.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(
        parsed.request_context.event_type.as_deref(),
        Some("MESSAGE")
    );
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_disconnect_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_disconnect.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(
        parsed.request_context.event_type.as_deref(),
        Some("DISCONNECT")
    );
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}
