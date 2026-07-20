//! broker-wasm — the resource-broker e2e guest.
//!
//! A `wasm32-wasip1` handler that exercises the `riz_broker` host imports
//! from inside the WASI sandbox: on every request it runs a parameterized
//! Postgres query through the broker (`pg_query` against the grant named in
//! `?grant=`, default "db") and returns the broker's response envelope as
//! the HTTP body — success rows or the structured error (`denied`,
//! `timeout`, `throttled`, …), exactly as the host handed it over.
//!
//! The guest never opens a socket, never sees a DSN: it names a grant and
//! gets bytes. Same stdin/stdout envelope as every other riz runtime.

use std::io::{self, BufRead, Write};

#[link(wasm_import_module = "riz_capability")]
extern "C" {
    /// Capability ABI v2: ONE dispatcher import for every brokered verb.
    /// Runs the call; the JSON response (ok or error envelope) is stashed
    /// host-side. Returns its length, or -1 for an ABI fault.
    fn call(
        verb_ptr: *const u8,
        verb_len: i32,
        grant_ptr: *const u8,
        grant_len: i32,
        req_ptr: *const u8,
        req_len: i32,
    ) -> i32;
    /// Copy the stashed response into `dst` and clear the stash. Returns the
    /// length; if `dst_cap` was too small, copies nothing and returns the
    /// needed length.
    fn read_response(dst_ptr: *mut u8, dst_cap: i32) -> i32;
}

fn brokered_pg_query(grant: &str, request: &serde_json::Value) -> String {
    let verb = "pg.query";
    let req = request.to_string();
    let len = unsafe {
        call(
            verb.as_ptr(),
            verb.len() as i32,
            grant.as_ptr(),
            grant.len() as i32,
            req.as_ptr(),
            req.len() as i32,
        )
    };
    if len < 0 {
        return r#"{"ok":false,"error":{"code":"abi","message":"call returned -1"}}"#.into();
    }
    let mut buf = vec![0u8; len as usize];
    let written = unsafe { read_response(buf.as_mut_ptr(), buf.len() as i32) };
    if written != len {
        return r#"{"ok":false,"error":{"code":"abi","message":"read_response length mismatch"}}"#
            .into();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

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
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
    let event = parsed.get("event").unwrap_or(&parsed);

    // ?grant=NAME selects the grant (default "db") — lets one guest binary
    // prove both the granted path and the deny-by-default path.
    let grant = event
        .get("queryStringParameters")
        .and_then(|q| q.get("grant"))
        .and_then(|g| g.as_str())
        .unwrap_or("db")
        .to_string();
    // ?sql=... overrides the query (the e2e uses the default).
    let sql = event
        .get("queryStringParameters")
        .and_then(|q| q.get("sql"))
        .and_then(|s| s.as_str())
        .unwrap_or("select id, status from orders")
        .to_string();

    let broker_response = brokered_pg_query(
        &grant,
        &serde_json::json!({ "sql": sql, "params": [] }),
    );

    // The broker envelope IS the body — the test asserts on it directly.
    let ok = serde_json::from_str::<serde_json::Value>(&broker_response)
        .ok()
        .and_then(|v| v.get("ok").and_then(|b| b.as_bool()))
        .unwrap_or(false);
    serde_json::json!({
        "statusCode": if ok { 200 } else { 502 },
        "headers": { "content-type": "application/json" },
        "cookies": [],
        "body": broker_response,
        "isBase64Encoded": false
    })
    .to_string()
}
