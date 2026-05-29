// Rust WebSocket handler — mirrors examples/lambdas/chat/index.ts (Bun).
//
// Receives all three AWS lifecycle event types ($connect, $disconnect,
// $default) as an `ApiGatewayWebsocketProxyRequest`. For $default it
// POSTs an echoed reply to the local @connections management endpoint
// so the message round-trips back to the WS client.
//
// RIZ_TEST_BASE_URL env var overrides the @connections base for
// integration tests that bind to ephemeral ports.

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
