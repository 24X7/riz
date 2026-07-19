//! wasm-rust — an AWS API Gateway v2 HTTP handler compiled to `wasm32-wasip1`
//! and run by riz inside wasmtime's WASI capability sandbox (deny-by-default:
//! no filesystem, network, or host env unless the function's config grants it).
//!
//! Authored as a pure Lambda handler on `riz-wasm` — the shim owns the event
//! loop and the capability ABI; this file owns nothing but the handler.

use riz_wasm::{Context, Error, Event, Response};

fn main() {
    riz_wasm::run(handler)
}

fn handler(event: Event, ctx: Context) -> Result<Response, Error> {
    let event = event.raw();
    let name = event
        .pointer("/queryStringParameters/name")
        .and_then(|v| v.as_str())
        .unwrap_or("world");
    let method = event
        .pointer("/requestContext/http/method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET");

    let body = serde_json::json!({
        "message": format!("hello, {name}"),
        "method": method,
        "path": event.get("rawPath").cloned().unwrap_or_default(),
        "functionName": ctx.function_name(),
        "awsRequestId": ctx.request_id(),
        "remainingMs": ctx.remaining_time().as_millis() as u64,
        "runtime": "wasm",
    });

    Ok(Response::from(serde_json::json!({
        "statusCode": 200,
        "headers": { "content-type": "application/json" },
        "body": body.to_string(),
        "isBase64Encoded": false,
    })))
}

// Reach Postgres (and more) without ever holding a credential: declare a
// capability grant in riz.toml, then call the typed client —
//
//   let rows = riz_wasm::cap::pg::query("db", "select 1 as one", &[])?;
//
// The host performs the I/O under the grant's limits; the guest only ever
// sees rows or a closed error set (denied/throttled/timeout/…).
