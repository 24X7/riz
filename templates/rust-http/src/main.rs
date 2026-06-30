// AWS API Gateway v2 HTTP Lambda handler in Rust, using the OFFICIAL AWS Lambda
// Rust runtime (`lambda_runtime`). No riz library — this exact binary runs
// unmodified on AWS Lambda and on riz (riz speaks the AWS Lambda Runtime API).
//
// Build: `cargo build --release`, then point riz.toml's handler at
// `./target/release/hello`.

use aws_lambda_events::apigw::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use http::HeaderMap;
use lambda_runtime::{run, service_fn, Error, LambdaEvent};

async fn handler(
    event: LambdaEvent<ApiGatewayV2httpRequest>,
) -> Result<ApiGatewayV2httpResponse, Error> {
    let (req, ctx) = (event.payload, event.context);
    let name = req
        .query_string_parameters
        .first("name")
        .unwrap_or("world")
        .to_string();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let body = serde_json::json!({
        "message": format!("hello, {name}"),
        "method": req.request_context.http.method.as_str(),
        "path": req.raw_path,
        "functionName": ctx.env_config.function_name,
        "awsRequestId": ctx.request_id,
        "remainingMs": (ctx.deadline as i64 - now_ms).max(0),
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

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}
