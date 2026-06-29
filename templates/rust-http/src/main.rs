// AWS API Gateway v2 HTTP Lambda handler in Rust.
// Runs on riz (https://riz.dev) via the riz-rust-runtime helper crate —
// no special host wrapper; the binary is the lambda.
//
// Build: `cargo build --release` then point riz.toml's handler at
// `./target/release/hello`.

use aws_lambda_events::apigw::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use http::HeaderMap;
use riz_rust_runtime::{run, Context};

async fn handler(
    event: ApiGatewayV2httpRequest,
    ctx: Context,
) -> Result<ApiGatewayV2httpResponse, Box<dyn std::error::Error + Send + Sync>> {
    let name = event
        .query_string_parameters
        .first("name")
        .unwrap_or("world")
        .to_string();
    let body = serde_json::json!({
        "message": format!("hello, {name}"),
        "method": event.request_context.http.method.as_str(),
        "path": event.raw_path,
        "functionName": ctx.function_name,
        "awsRequestId": ctx.aws_request_id,
        "remainingMs": ctx.get_remaining_time_in_millis(),
    });

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());

    Ok(ApiGatewayV2httpResponse {
        status_code: 200,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(aws_lambda_events::encodings::Body::Text(body.to_string())),
        is_base64_encoded: false,
        cookies: Vec::new(),
    })
}

fn main() {
    run(handler);
}
