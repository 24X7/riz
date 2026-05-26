//! Wave 1 — WebSocket APIs acceptance criteria.

#[test]
fn protocol_websocket_parses() {
    let toml_str = r#"
[server]
port = 3000
host = "0.0.0.0"

[function.chat]
runtime = "bun"
handler = "./chat.handler"
protocol = "websocket"
"#;
    let cfg: riz::config::Config = toml::from_str(toml_str).unwrap();
    let f = cfg.functions.get("chat").unwrap();
    assert_eq!(f.protocol, riz::config::Protocol::WebSocket);
    // Already shipped at WS Task 2 — #[ignore] removed.
}

#[test]
#[ignore = "wave 1 not yet shipped: $connect dispatches an ApiGatewayWebsocketProxyRequest"]
fn websocket_connect_dispatches_proxy_request() {
    // Implementer fills this in during WS Task 7.
}

#[test]
#[ignore = "wave 1 not yet shipped: $default invoked per message"]
fn websocket_default_dispatches_per_message() {}

#[test]
#[ignore = "wave 1 not yet shipped: $disconnect dispatches on close"]
fn websocket_disconnect_dispatches_on_close() {}

#[test]
#[ignore = "wave 1 not yet shipped: connectionId is present in requestContext"]
fn websocket_connection_id_populated() {}

#[test]
#[ignore = "wave 1 not yet shipped: POST /_riz/connections/{id} sends to client"]
fn connections_post_sends_to_client() {}

#[test]
#[ignore = "wave 1 not yet shipped: DELETE /_riz/connections/{id} closes connection"]
fn connections_delete_closes_connection() {}

#[test]
#[ignore = "wave 1 not yet shipped: GET /_riz/connections/{id} inspects connection"]
fn connections_get_inspects_connection() {}

#[test]
#[ignore = "wave 1 not yet shipped: connections survive hot-reload of the ws function"]
fn websocket_connections_survive_hot_reload() {}

#[test]
#[ignore = "wave 1 not yet shipped: all connections cleanly closed on SIGTERM within 30s"]
fn websocket_clean_close_on_sigterm() {}
