//! wasm-http — a minimal AWS API Gateway v2 HTTP handler compiled to
//! `wasm32-wasip1` and run by riz inside wasmtime's WASI capability sandbox.
//!
//! The contract is the same line-delimited JSON envelope every riz runtime
//! speaks: read one `{ event, __riz_deadline_ms, __riz_function_name }` line
//! from stdin, write one gateway-shaped response line to stdout. Pure sync std
//! — no tokio, no networking — so it compiles to wasm cleanly and runs
//! deny-by-default (stdio only; no filesystem, network, or host env unless the
//! function's config grants it).

use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let _ = writeln!(stdout, "{}", handle(&line));
        let _ = stdout.flush();
    }
}

fn handle(line: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
        return error(400, "bad event json");
    };
    // Envelope: { event, __riz_function_name, ... } — fall back to a bare event
    // for manual `echo '{...}' | riz ...` style invocations.
    let event = parsed.get("event").unwrap_or(&parsed);
    let function_name = parsed
        .get("__riz_function_name")
        .and_then(|v| v.as_str())
        .unwrap_or("hello");

    let name = event
        .get("queryStringParameters")
        .and_then(|q| q.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("world");
    let method = event
        .get("requestContext")
        .and_then(|rc| rc.get("http"))
        .and_then(|h| h.get("method"))
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
    let request_id = event
        .get("requestContext")
        .and_then(|rc| rc.get("requestId"))
        .and_then(|v| v.as_str())
        .unwrap_or("local");

    let body = serde_json::json!({
        "message": format!("hello, {name}"),
        "method": method,
        "functionName": function_name,
        "awsRequestId": request_id,
        "runtime": "wasm",
    });

    serde_json::json!({
        "statusCode": 200,
        "headers": { "content-type": "application/json" },
        "multiValueHeaders": {},
        "body": body.to_string(),
        "isBase64Encoded": false,
        "cookies": [],
    })
    .to_string()
}

fn error(status: u16, message: &str) -> String {
    serde_json::json!({
        "statusCode": status,
        "headers": { "content-type": "application/json" },
        "multiValueHeaders": {},
        "body": serde_json::json!({ "message": message }).to_string(),
        "isBase64Encoded": false,
        "cookies": [],
    })
    .to_string()
}
