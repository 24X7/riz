//! Builders for `ApiGatewayWebsocketProxyRequest` events — one builder per
//! AWS WebSocket lifecycle event type.

use crate::gateway::{ApiGatewayWebsocketProxyRequest, ApiGatewayWebsocketProxyRequestContext};
use http::HeaderMap;
use std::collections::HashMap;
use std::time::SystemTime;

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn base_context(
    stage: &str,
    connection_id: &str,
    connected_at_ms: i64,
    event_type: &str,
    route_key: &str,
) -> ApiGatewayWebsocketProxyRequestContext {
    ApiGatewayWebsocketProxyRequestContext {
        account_id: Some("riz".into()),
        stage: Some(stage.into()),
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        connection_id: Some(connection_id.into()),
        connected_at: connected_at_ms,
        event_type: Some(event_type.into()),
        route_key: Some(route_key.into()),
        request_time_epoch: epoch_ms(),
        ..Default::default()
    }
}

pub fn build_connect(
    stage: &str,
    connection_id: &str,
    connected_at_ms: i64,
    upgrade_path: &str,
    headers: HeaderMap,
    query: HashMap<String, String>,
) -> ApiGatewayWebsocketProxyRequest {
    let ctx = base_context(stage, connection_id, connected_at_ms, "CONNECT", "$connect");
    ApiGatewayWebsocketProxyRequest {
        resource: Some(upgrade_path.to_string()),
        path: Some(upgrade_path.to_string()),
        http_method: Some(http::Method::GET),
        headers,
        multi_value_headers: HeaderMap::new(),
        query_string_parameters: query.into(),
        multi_value_query_string_parameters: Default::default(),
        path_parameters: Default::default(),
        stage_variables: Default::default(),
        request_context: ctx,
        body: None,
        is_base64_encoded: false,
    }
}

pub fn build_message(
    stage: &str,
    connection_id: &str,
    connected_at_ms: i64,
    body: Option<String>,
    is_base64_encoded: bool,
) -> ApiGatewayWebsocketProxyRequest {
    let mut ctx = base_context(stage, connection_id, connected_at_ms, "MESSAGE", "$default");
    ctx.message_id = Some(uuid::Uuid::new_v4().to_string());
    ApiGatewayWebsocketProxyRequest {
        resource: None,
        path: None,
        http_method: None,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        query_string_parameters: Default::default(),
        multi_value_query_string_parameters: Default::default(),
        path_parameters: Default::default(),
        stage_variables: Default::default(),
        request_context: ctx,
        body,
        is_base64_encoded,
    }
}

pub fn build_disconnect(
    stage: &str,
    connection_id: &str,
    connected_at_ms: i64,
) -> ApiGatewayWebsocketProxyRequest {
    let ctx = base_context(
        stage,
        connection_id,
        connected_at_ms,
        "DISCONNECT",
        "$disconnect",
    );
    ApiGatewayWebsocketProxyRequest {
        resource: None,
        path: None,
        http_method: None,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        query_string_parameters: Default::default(),
        multi_value_query_string_parameters: Default::default(),
        path_parameters: Default::default(),
        stage_variables: Default::default(),
        request_context: ctx,
        body: None,
        is_base64_encoded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_event_has_correct_routekey_and_eventtype() {
        let ev = build_connect(
            "$default",
            "abc-123",
            0,
            "/chat",
            HeaderMap::new(),
            HashMap::new(),
        );
        assert_eq!(ev.request_context.event_type.as_deref(), Some("CONNECT"));
        assert_eq!(ev.request_context.route_key.as_deref(), Some("$connect"));
        assert_eq!(ev.request_context.connection_id.as_deref(), Some("abc-123"));
        assert_eq!(ev.path.as_deref(), Some("/chat"));
    }

    #[test]
    fn message_event_has_message_id_and_default_routekey() {
        let ev = build_message("$default", "abc-123", 0, Some("hello".into()), false);
        assert_eq!(ev.request_context.event_type.as_deref(), Some("MESSAGE"));
        assert_eq!(ev.request_context.route_key.as_deref(), Some("$default"));
        assert!(ev.request_context.message_id.is_some());
        assert_eq!(ev.body.as_deref(), Some("hello"));
    }

    #[test]
    fn disconnect_event_has_correct_eventtype() {
        let ev = build_disconnect("$default", "abc-123", 0);
        assert_eq!(ev.request_context.event_type.as_deref(), Some("DISCONNECT"));
        assert_eq!(ev.request_context.route_key.as_deref(), Some("$disconnect"));
        assert!(ev.body.is_none());
    }

    #[test]
    fn serializes_to_aws_wire_format() {
        let ev = build_connect(
            "$default",
            "abc-123",
            1000,
            "/chat",
            HeaderMap::new(),
            HashMap::new(),
        );
        let json: serde_json::Value = serde_json::to_value(&ev).unwrap();
        // AWS uses camelCase on the wire; verify a couple of the renames.
        assert_eq!(json["requestContext"]["connectionId"], "abc-123");
        assert_eq!(json["requestContext"]["eventType"], "CONNECT");
        assert_eq!(json["requestContext"]["routeKey"], "$connect");
    }
}
