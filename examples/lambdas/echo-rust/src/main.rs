use aws_lambda_events::apigw::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use http::HeaderMap;
use riz_rust_runtime::{run, Context};

async fn handler(
    event: ApiGatewayV2httpRequest,
    ctx: Context,
) -> Result<ApiGatewayV2httpResponse, Box<dyn std::error::Error + Send + Sync>> {
    // FOOTGUN: aws_lambda_events::query_map::QueryMap deserializes correctly
    // from the AWS v2 single-string shape (`{"name": "alice"}`) BUT its
    // default Serialize impl emits the multi-value shape (`{"name": ["alice"]}`)
    // when used through serde_json::json! / serde_json::Value. The field-level
    // `serialize_with = aws_api_gateway_v2::serialize_query_string_parameters`
    // hint on ApiGatewayV2httpRequest only applies when serializing the WHOLE
    // struct — not when re-serializing the field standalone.
    //
    // Flatten manually to single-value form so the response body matches what
    // the Bun and Python adapters emit. Every Rust Lambda handler that needs
    // to re-emit queryStringParameters faces this exact gotcha.
    let qs_flat: std::collections::HashMap<String, String> = event
        .query_string_parameters
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    // HeaderMap doesn't directly serialize as a JSON object via serde_json::json!
    // — flatten to a HashMap<String, String> of lowercased name → first value.
    let headers_flat: std::collections::HashMap<String, String> = event
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let body = serde_json::json!({
        "echo": event.raw_path,
        "method": event.request_context.http.method.as_str(),
        "functionName": ctx.function_name,
        "invokedFunctionArn": ctx.invoked_function_arn,
        "awsRequestId": ctx.aws_request_id,
        "remainingMs": ctx.get_remaining_time_in_millis(),
        "body": event.body,
        "isBase64Encoded": event.is_base64_encoded,
        "pathParameters": event.path_parameters,
        "queryStringParameters": qs_flat,
        "stageVariables": event.stage_variables,
        "cookies": event.cookies,
        "requestHeaders": headers_flat,
    });
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert("content-type", "application/json".parse().unwrap());
    resp_headers.insert("x-riz-echo", "ok".parse().unwrap());

    Ok(ApiGatewayV2httpResponse {
        status_code: 200,
        headers: resp_headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(aws_lambda_events::encodings::Body::Text(body.to_string())),
        is_base64_encoded: false,
        cookies: vec!["sid=abc; Path=/".to_string()],
    })
}

fn main() {
    run(handler);
}
