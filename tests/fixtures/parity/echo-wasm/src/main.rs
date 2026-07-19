//! echo-wasm — the WASM member of the echo parity set.
//!
//! Authored as a pure Lambda handler on `riz-wasm`: APIGW v2 event in, Lambda
//! proxy response out. The shim owns the wire; this file owns nothing but the
//! handler. Compiled to `wasm32-wasip1` and run by `riz __wasm-host` inside
//! wasmtime's WASI capability sandbox, it returns the same canonical echo
//! response as the bun / node / python / rust echo handlers — proving a
//! `.wasm` handler is a first-class riz runtime.

use riz_wasm::{Context, Error, Event, Response};

fn main() {
    riz_wasm::run(handler)
}

fn handler(event: Event, ctx: Context) -> Result<Response, Error> {
    let event = event.raw();
    let function_name = ctx.function_name();
    let remaining = ctx.remaining_time().as_millis() as i64;

    // Honor ?status=NNN for the error-status parity test.
    let status = event
        .get("queryStringParameters")
        .and_then(|q| q.get("status"))
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(200);

    let arn = format!("arn:riz:lambda:local:000000000000:function:{function_name}");
    let request_id = if ctx.request_id().is_empty() {
        format!("req-{}", ctx.deadline_ms())
    } else {
        ctx.request_id().to_string()
    };
    let method = event
        .get("requestContext")
        .and_then(|rc| rc.get("http"))
        .and_then(|h| h.get("method"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let body = serde_json::json!({
        "echo": event.get("rawPath").cloned().unwrap_or_else(|| serde_json::Value::String(String::new())),
        "method": method,
        "functionName": function_name,
        "invokedFunctionArn": arn,
        "awsRequestId": request_id,
        "remainingMs": remaining,
        "body": event.get("body").cloned().unwrap_or(serde_json::Value::Null),
        "isBase64Encoded": event.get("isBase64Encoded").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "pathParameters": event.get("pathParameters").cloned().unwrap_or(serde_json::Value::Null),
        "queryStringParameters": event.get("queryStringParameters").cloned().unwrap_or(serde_json::Value::Null),
        "stageVariables": event.get("stageVariables").cloned().unwrap_or(serde_json::Value::Null),
        "cookies": event.get("cookies").cloned().unwrap_or(serde_json::Value::Null),
        "requestHeaders": event.get("headers").cloned().unwrap_or(serde_json::Value::Null),
        "authorizer": event.get("requestContext").and_then(|rc| rc.get("authorizer")).cloned().unwrap_or(serde_json::Value::Null),
    });

    Ok(Response::from(serde_json::json!({
        "statusCode": status,
        "headers": { "content-type": "application/json", "x-riz-echo": "ok" },
        "multiValueHeaders": {},
        "body": body.to_string(),
        "isBase64Encoded": false,
        "cookies": ["sid=abc; Path=/"],
    })))
}
