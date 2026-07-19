// echo-rust — the Rust leg of riz's cross-runtime parity matrix.
//
// Written with the OFFICIAL AWS Lambda Rust runtime (`lambda_runtime`) — there
// is NO riz library. This exact binary runs unmodified on AWS Lambda and on
// riz, because riz implements the AWS Lambda Runtime API. Emits the canonical
// echo shape shared by echo-bun / echo-node / echo-python / echo-go.

use aws_lambda_events::apigw::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use http::HeaderMap;
use lambda_runtime::{run, service_fn, Error, LambdaEvent};

async fn handler(
    event: LambdaEvent<ApiGatewayV2httpRequest>,
) -> Result<ApiGatewayV2httpResponse, Error> {
    let (req, ctx) = (event.payload, event.context);

    // queryStringParameters: flatten the single-value AWS v2 shape (the default
    // Serialize of QueryMap emits the multi-value form when re-serialized).
    let qs_flat: std::collections::HashMap<String, String> = req
        .query_string_parameters
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let headers_flat: std::collections::HashMap<String, String> = req
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    // remainingMs from the Runtime-API deadline (Unix-millis), like AWS.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let remaining_ms = (ctx.deadline as i64 - now_ms).max(0);

    let body = serde_json::json!({
        "echo": req.raw_path,
        "method": req.request_context.http.method.as_str(),
        "functionName": ctx.env_config.function_name,
        "invokedFunctionArn": ctx.invoked_function_arn,
        "awsRequestId": ctx.request_id,
        "remainingMs": remaining_ms,
        "body": req.body,
        "isBase64Encoded": req.is_base64_encoded,
        "pathParameters": req.path_parameters,
        "queryStringParameters": qs_flat,
        "stageVariables": req.stage_variables,
        "cookies": req.cookies,
        "requestHeaders": headers_flat,
    });

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert("content-type", "application/json".parse().unwrap());
    resp_headers.insert("x-riz-echo", "ok".parse().unwrap());

    // Honor ?status=NNN for the parity error-status test.
    let status_code: i64 = req
        .query_string_parameters
        .first("status")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    Ok(ApiGatewayV2httpResponse {
        status_code,
        headers: resp_headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(aws_lambda_events::encodings::Body::Text(body.to_string())),
        is_base64_encoded: false,
        cookies: vec!["sid=abc; Path=/".to_string()],
    })
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}
