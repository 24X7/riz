// AWS API Gateway v2 WebSocket Lambda handler in Rust.
// Runs on riz (https://riz.dev) via the riz-rust-runtime helper crate —
// no special host wrapper; the binary is the lambda.
//
// Three lifecycle events arrive at this single handler, distinguished
// by event.request_context.route_key:
//   $connect    — when a client opens the socket
//   $disconnect — when the client (or server) closes the socket
//   $default    — for every message the client sends
//
// Build: `cargo build --release`. Then `riz run`.

use aws_lambda_events::apigw::ApiGatewayWebsocketProxyRequest;
use riz_rust_runtime::{run, Context};
use serde::Serialize;

#[derive(Serialize)]
struct WsResponse {
    #[serde(rename = "statusCode")]
    status_code: u16,
}

async fn handler(
    event: ApiGatewayWebsocketProxyRequest,
    _ctx: Context,
) -> Result<WsResponse, Box<dyn std::error::Error + Send + Sync>> {
    let route = event.request_context.route_key.as_deref();
    let conn_id = event
        .request_context
        .connection_id
        .clone()
        .unwrap_or_default();

    if matches!(route, Some("$connect") | Some("$disconnect")) {
        return Ok(WsResponse { status_code: 200 });
    }

    // $default: POST the echoed message back via @connections.
    let base = std::env::var("RIZ_TEST_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".into());
    let body_in = event.body.unwrap_or_default();
    let payload = format!("echo: {body_in}");

    let _ = reqwest::Client::new()
        .post(format!("{base}/_riz/connections/{conn_id}"))
        .body(payload)
        .send()
        .await;

    Ok(WsResponse { status_code: 200 })
}

fn main() {
    run(handler);
}
