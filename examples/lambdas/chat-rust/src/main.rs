// Rust WebSocket handler — mirrors examples/lambdas/chat/index.ts (Bun).
//
// Written with the OFFICIAL AWS Lambda Rust runtime (`lambda_runtime`) — no riz
// library. Receives the AWS lifecycle event types ($connect, $disconnect,
// $default) as an `ApiGatewayWebsocketProxyRequest`. For $default it POSTs an
// echoed reply to the local @connections management endpoint so the message
// round-trips back to the WS client.
//
// RIZ_TEST_BASE_URL overrides the @connections base for integration tests that
// bind to ephemeral ports.

use aws_lambda_events::apigw::ApiGatewayWebsocketProxyRequest;
use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde::Serialize;

#[derive(Serialize)]
struct WsResponse {
    #[serde(rename = "statusCode")]
    status_code: u16,
}

async fn handler(event: LambdaEvent<ApiGatewayWebsocketProxyRequest>) -> Result<WsResponse, Error> {
    let req = event.payload;
    let route = req.request_context.route_key.as_deref();
    let conn_id = req
        .request_context
        .connection_id
        .clone()
        .unwrap_or_default();

    if matches!(route, Some("$connect") | Some("$disconnect")) {
        return Ok(WsResponse { status_code: 200 });
    }

    // $default: POST the echoed message back via @connections.
    let base =
        std::env::var("RIZ_TEST_BASE_URL").unwrap_or_else(|_| "http://localhost:3000".into());
    let body_in = req.body.unwrap_or_default();
    let payload = format!("echo: {body_in}");

    let _ = reqwest::Client::new()
        .post(format!("{base}/_riz/connections/{conn_id}"))
        .body(payload)
        .send()
        .await;

    Ok(WsResponse { status_code: 200 })
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}
