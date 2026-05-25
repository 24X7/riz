//! Re-exports of the canonical AWS HTTP API Gateway v2 event/response types.
//!
//! Riz uses these exact shapes so handlers written for real AWS Lambda run
//! on Riz unchanged — same crate (`aws_lambda_events`), same field names,
//! same serde JSON format on the wire to the child process.

pub use aws_lambda_events::apigw::{
    ApiGatewayV2httpRequest,
    ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
    ApiGatewayV2httpResponse,
};
pub use aws_lambda_events::encodings::Body;
