//! echo-wasm — the WASM member of the echo parity set.
//!
//! Compiled to `wasm32-wasip1` and run by `riz __wasm-host` inside wasmtime's
//! WASI capability sandbox. It reads the same line-delimited JSON envelope from
//! stdin and writes the same canonical echo response to stdout as the
//! bun / node / python / rust echo handlers — proving a `.wasm` handler is a
//! first-class riz runtime.
//!
//! Pure sync std — no tokio, no networking — so it compiles to wasm cleanly.

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let _ = writeln!(stdout, "{}", handle(&line));
        let _ = stdout.flush();
    }
}

fn handle(line: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return error_400(),
    };
    // Envelope: { event, __riz_deadline_ms, __riz_function_name } — fall back to
    // a bare event for manual invocations.
    let event = parsed.get("event").unwrap_or(&parsed);
    let function_name = parsed
        .get("__riz_function_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let deadline_ms = parsed
        .get("__riz_deadline_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let remaining = (deadline_ms - now_ms).max(0);

    // Honor ?status=NNN for the error-status parity test.
    let status = event
        .get("queryStringParameters")
        .and_then(|q| q.get("status"))
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(200);

    let arn = format!("arn:riz:lambda:local:000000000000:function:{function_name}");
    let request_id = match event
        .get("requestContext")
        .and_then(|rc| rc.get("requestId"))
        .and_then(|v| v.as_str())
    {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("req-{deadline_ms}"),
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

    serde_json::json!({
        "statusCode": status,
        "headers": { "content-type": "application/json", "x-riz-echo": "ok" },
        "multiValueHeaders": {},
        "body": body.to_string(),
        "isBase64Encoded": false,
        "cookies": ["sid=abc; Path=/"],
    })
    .to_string()
}

fn error_400() -> String {
    serde_json::json!({
        "statusCode": 400,
        "headers": { "content-type": "application/json" },
        "multiValueHeaders": {},
        "body": "{\"message\":\"bad event json\"}",
        "isBase64Encoded": false,
        "cookies": [],
    })
    .to_string()
}
