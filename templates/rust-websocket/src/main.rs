// AWS API Gateway v2 WebSocket Lambda handler in Rust, using the OFFICIAL AWS
// Lambda Rust runtime (`lambda_runtime`). No riz library — this exact binary
// runs unmodified on AWS Lambda and on riz (riz speaks the AWS Lambda Runtime
// API).
//
// Three lifecycle events arrive at this single handler, distinguished by
// event.request_context.route_key:
//   $connect    — when a client opens the socket
//   $disconnect — when the client (or server) closes the socket
//   $default    — for every message the client sends
//
// Build: `cargo build --release`. Then `riz run`.

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
