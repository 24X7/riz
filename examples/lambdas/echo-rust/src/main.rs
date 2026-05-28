use aws_lambda_events::apigw::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use http::HeaderMap;
use riz_rust_runtime::{run, Context};

async fn handler(
    event: ApiGatewayV2httpRequest,
    ctx: Context,
) -> Result<ApiGatewayV2httpResponse, Box<dyn std::error::Error + Send + Sync>> {
    let body = serde_json::json!({
        "echo": event.raw_path,
        "method": event.request_context.http.method.as_str(),
        "functionName": ctx.function_name,
        "invokedFunctionArn": ctx.invoked_function_arn,
        "awsRequestId": ctx.aws_request_id,
        "remainingMs": ctx.get_remaining_time_in_millis(),
    });
    Ok(ApiGatewayV2httpResponse {
        status_code: 200,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        body: Some(aws_lambda_events::encodings::Body::Text(body.to_string())),
        is_base64_encoded: false,
        cookies: Vec::new(),
    })
}

fn main() {
    run(handler);
}
