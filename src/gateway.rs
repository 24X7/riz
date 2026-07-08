//! Re-exports of the canonical AWS HTTP API Gateway v2 event/response types.
//!
//! Riz uses these exact shapes so handlers written for real AWS Lambda run
//! on Riz unchanged — same crate (`aws_lambda_events`), same field names,
//! same serde JSON format on the wire to the child process.

// The WebSocket upgrade handler (src/ws/) and the integration tests both
// consume these; keep the re-exports stable so handler authors and tests can
// rely on `riz::gateway::*`.
pub use aws_lambda_events::apigw::{
    ApiGatewayProxyResponse,
    ApiGatewayV2httpRequest,
    ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
    ApiGatewayV2httpResponse,
    // WebSocket request envelope + per-route response shape (the AWS
    // WebSocket return type happens to reuse the v1 REST response struct).
    ApiGatewayWebsocketProxyRequest,
    ApiGatewayWebsocketProxyRequestContext,
};
pub use aws_lambda_events::encodings::Body;
