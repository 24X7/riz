//! riz-wasm — author an AWS-Lambda-shaped handler; the shim owns the wire.
//!
//! A guest's whole `main.rs` is:
//!
//! ```ignore
//! fn handler(event: riz_wasm::Event, ctx: riz_wasm::Context)
//!     -> Result<riz_wasm::Response, riz_wasm::Error> {
//!     Ok(serde_json::json!({ "statusCode": 200, "body": "hi" }).into())
//! }
//! fn main() { riz_wasm::run(handler) }
//! ```
//!
//! Wire v1 (selected when `RIZ_WIRE` is unset or `"1"`): one JSON line per
//! invocation on stdin — `{ event, __riz_deadline_ms, __riz_function_name }`
//! with a bare-event fallback — and one Lambda proxy-response JSON line on
//! stdout. An unknown `RIZ_WIRE` value fails closed at startup (exit 78) so
//! wire skew can never desync the pipe.
//!
//! Capability calls go through [`cap`] — typed wrappers over the host's
//! `riz_capability` imports. Authors never write an event loop, touch stdin, or
//! declare `unsafe extern` blocks.

use std::io::{BufRead, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Handler error type — anything printable. The shim converts an `Err` into a
/// canonical 500 Lambda response line and keeps serving.
pub type Error = Box<dyn std::error::Error>;

/// The full AWS API Gateway v2 HTTP event, untyped.
pub struct Event(serde_json::Value);

impl Event {
    /// The raw APIGW v2 event object.
    pub fn raw(&self) -> &serde_json::Value {
        &self.0
    }
    /// Consume the wrapper, yielding the event value.
    pub fn into_inner(self) -> serde_json::Value {
        self.0
    }
}

/// The full Lambda proxy response object the handler wants sent.
pub struct Response(serde_json::Value);

impl From<serde_json::Value> for Response {
    fn from(v: serde_json::Value) -> Self {
        Response(v)
    }
}

/// Per-invocation context, mirroring Lambda's context object.
pub struct Context {
    function_name: String,
    request_id: String,
    deadline_ms: i64,
}

impl Context {
    /// The riz function name for this invocation (`"unknown"` for bare events).
    pub fn function_name(&self) -> &str {
        &self.function_name
    }
    /// From `event.requestContext.requestId`; empty string when the event
    /// lacks it.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// Absolute invocation deadline, unix milliseconds (0 for bare events).
    pub fn deadline_ms(&self) -> i64 {
        self.deadline_ms
    }
    /// Time left before the deadline, clamped at zero.
    pub fn remaining_time(&self) -> Duration {
        let remaining = self.deadline_ms.saturating_sub(now_millis());
        Duration::from_millis(remaining.max(0) as u64)
    }
}

/// A riz WASM handler: APIGW v2 event in, Lambda proxy response out.
pub type Handler = fn(Event, Context) -> Result<Response, Error>;

/// Exported ABI marker — the host's load-time handshake signal (checked
/// fail-closed once wire negotiation ships). Also the artifact-level
/// conformance signal: a `.wasm` built on riz-wasm carries this export.
#[no_mangle]
pub extern "C" fn riz_abi_v1() {}

/// Run the handler for the life of the process. Owns the wire: envelope
/// parsing, context synthesis, response framing, and error fallbacks.
pub fn run(handler: Handler) -> ! {
    match std::env::var("RIZ_WIRE") {
        Err(_) => {}
        Ok(v) if v == "1" => {}
        Ok(other) => {
            eprintln!(
                "riz-wasm: unsupported RIZ_WIRE={other:?}; this shim speaks wire v1 only — \
                 upgrade the riz-wasm dependency and rebuild"
            );
            std::process::exit(78);
        }
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let _ = writeln!(stdout, "{}", process_line(&line, handler));
        let _ = stdout.flush();
    }
    std::process::exit(0)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One invocation: envelope in, response line out. Pure except for the
/// handler call, so the whole wire contract is unit-testable.
fn process_line(line: &str, handler: Handler) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
        return error_line(400, "bad event json");
    };
    let (event, function_name, deadline_ms) = split_envelope(parsed);
    let request_id = event
        .get("requestContext")
        .and_then(|rc| rc.get("requestId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ctx = Context {
        function_name,
        request_id,
        deadline_ms,
    };
    match handler(Event(event), ctx) {
        Ok(Response(v)) => v.to_string(),
        Err(e) => error_line(500, &format!("handler error: {e}")),
    }
}

/// Envelope: `{ event, __riz_deadline_ms, __riz_function_name }`, falling back
/// to treating the whole line as a bare event for manual invocations.
fn split_envelope(mut parsed: serde_json::Value) -> (serde_json::Value, String, i64) {
    let function_name = parsed
        .get("__riz_function_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let deadline_ms = parsed
        .get("__riz_deadline_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let event = match parsed.get_mut("event") {
        Some(e) => e.take(),
        None => parsed,
    };
    (event, function_name, deadline_ms)
}

/// The canonical error response line (same shape every riz runtime emits).
fn error_line(status: i64, message: &str) -> String {
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

/// Brokered host capabilities — typed wrappers over the `riz_capability`
/// dispatcher import. Every capability is a verb; the raw ABI dance lives in
/// [`raw_call`], and each submodule builds a typed request and decodes the
/// response envelope.
pub mod cap {
    use std::fmt;

    /// The broker's closed error set, surfaced verbatim: `denied`,
    /// `throttled`, `timeout`, `too_large`, `bad_request`, `backend`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CapError {
        pub code: String,
        pub message: String,
    }

    impl fmt::Display for CapError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}: {}", self.code, self.message)
        }
    }

    impl std::error::Error for CapError {}

    pub(crate) fn local_err(code: &str, message: impl Into<String>) -> CapError {
        CapError {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// The one dispatcher call every verb goes through: send `(verb, grant,
    /// body)` to the host and return the raw response envelope bytes.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn raw_call(verb: &str, grant: &str, body: &[u8]) -> Result<Vec<u8>, CapError> {
        #[link(wasm_import_module = "riz_capability")]
        extern "C" {
            fn call(
                verb_ptr: *const u8,
                verb_len: usize,
                grant_ptr: *const u8,
                grant_len: usize,
                req_ptr: *const u8,
                req_len: usize,
            ) -> i32;
            fn read_response(dst_ptr: *mut u8, dst_cap: usize) -> i32;
        }
        // The host bounds-checks every (ptr,len) pair; -1 signals an ABI
        // fault, any other value is the stashed response length.
        let n = unsafe {
            call(
                verb.as_ptr(),
                verb.len(),
                grant.as_ptr(),
                grant.len(),
                body.as_ptr(),
                body.len(),
            )
        };
        if n < 0 {
            return Err(local_err("bad_request", "broker ABI fault"));
        }
        let mut buf = vec![0u8; n as usize];
        let got = unsafe { read_response(buf.as_mut_ptr(), buf.len()) };
        if got < 0 || got as usize != buf.len() {
            return Err(local_err(
                "bad_request",
                format!("broker stash read mismatch: expected {n}, got {got}"),
            ));
        }
        Ok(buf)
    }

    /// Host builds (tests, clippy) have no broker: fail closed with the
    /// broker's own vocabulary.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn raw_call(_verb: &str, _grant: &str, _body: &[u8]) -> Result<Vec<u8>, CapError> {
        Err(local_err(
            "denied",
            "capabilities are only available inside the riz wasm host",
        ))
    }

    /// Parse the success envelope's `rows` array, or the error envelope.
    fn envelope_rows(bytes: &[u8]) -> Result<Vec<serde_json::Value>, CapError> {
        let v: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| local_err("bad_request", format!("malformed broker response: {e}")))?;
        if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
            return Ok(v
                .get("rows")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default());
        }
        let code = v
            .pointer("/error/code")
            .and_then(|c| c.as_str())
            .unwrap_or("backend")
            .to_string();
        let message = v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("broker error")
            .to_string();
        Err(CapError { code, message })
    }

    /// Postgres through the host broker (`pg`-type capability grants).
    pub mod pg {
        pub use super::CapError;

        /// Decode a broker response envelope: `{"ok":true,"rows":[..]}` or
        /// `{"ok":false,"error":{"code":..,"message":..}}`.
        pub fn decode_response(bytes: &[u8]) -> Result<Vec<serde_json::Value>, CapError> {
            super::envelope_rows(bytes)
        }

        /// Run one parameterized query against a named `pg` grant. The host
        /// bounds the call and enforces the grant's limits; you get rows or the
        /// closed error set.
        pub fn query(
            grant: &str,
            sql: &str,
            params: &[serde_json::Value],
        ) -> Result<Vec<serde_json::Value>, CapError> {
            let req = serde_json::json!({ "sql": sql, "params": params }).to_string();
            let bytes = super::raw_call("pg.query", grant, req.as_bytes())?;
            super::envelope_rows(&bytes)
        }
    }

    /// Outbound HTTP through the host broker (`http`-type grants). The guest
    /// supplies a method + a path RELATIVE to the resource's `base_url`; the
    /// daemon pins the origin, injects auth, and blocks SSRF.
    pub mod http {
        pub use super::CapError;

        /// A brokered HTTP response.
        #[derive(Debug, Clone)]
        pub struct HttpResponse {
            pub status: u16,
            pub body: String,
            pub headers: serde_json::Value,
        }

        /// Perform one brokered request. `path` is relative to the grant's
        /// `base_url`; `method` must be allowed by the grant.
        pub fn fetch(
            grant: &str,
            method: &str,
            path: &str,
            body: Option<&str>,
        ) -> Result<HttpResponse, CapError> {
            let mut req = serde_json::json!({ "method": method, "path": path });
            if let Some(b) = body {
                req["body"] = serde_json::Value::from(b);
            }
            let bytes = super::raw_call("http.fetch", grant, req.to_string().as_bytes())?;
            let rows = super::envelope_rows(&bytes)?;
            let row = rows
                .into_iter()
                .next()
                .ok_or_else(|| super::local_err("backend", "http response envelope had no row"))?;
            let status = row.get("status").and_then(|s| s.as_u64()).unwrap_or(0) as u16;
            let body = row
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();
            let headers = row
                .get("headers")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(HttpResponse {
                status,
                body,
                headers,
            })
        }
    }

    /// DynamoDB through the host broker (`dynamo`-type grants). The guest sends
    /// item-level DynamoDB JSON (minus `TableName`, which the daemon injects)
    /// and never holds a key or a signature — the daemon SigV4-signs host-side.
    pub mod dynamo {
        pub use super::CapError;

        fn call(
            verb: &str,
            grant: &str,
            request: &serde_json::Value,
        ) -> Result<serde_json::Value, CapError> {
            let bytes = super::raw_call(verb, grant, request.to_string().as_bytes())?;
            let rows = super::envelope_rows(&bytes)?;
            let row = rows
                .into_iter()
                .next()
                .ok_or_else(|| super::local_err("backend", "dynamo response had no row"))?;
            Ok(row.get("body").cloned().unwrap_or(serde_json::Value::Null))
        }

        /// `GetItem` — `request` carries `Key` (DynamoDB JSON). Returns the
        /// DynamoDB response body (e.g. `{ "Item": { ... } }`).
        pub fn get_item(
            grant: &str,
            request: &serde_json::Value,
        ) -> Result<serde_json::Value, CapError> {
            call("dynamo.get_item", grant, request)
        }
        /// `PutItem` — `request` carries `Item`. (Denied on a read-only grant.)
        pub fn put_item(
            grant: &str,
            request: &serde_json::Value,
        ) -> Result<serde_json::Value, CapError> {
            call("dynamo.put_item", grant, request)
        }
        /// `Query` — `request` carries the key-condition expression.
        pub fn query(
            grant: &str,
            request: &serde_json::Value,
        ) -> Result<serde_json::Value, CapError> {
            call("dynamo.query", grant, request)
        }
        /// `DeleteItem` — `request` carries `Key`. (Denied on a read-only grant.)
        pub fn delete_item(
            grant: &str,
            request: &serde_json::Value,
        ) -> Result<serde_json::Value, CapError> {
            call("dynamo.delete_item", grant, request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_handler(event: Event, ctx: Context) -> Result<Response, Error> {
        Ok(serde_json::json!({
            "statusCode": 200,
            "fn": ctx.function_name(),
            "rid": ctx.request_id(),
            "deadline": ctx.deadline_ms(),
            "path": event.raw().get("rawPath").cloned().unwrap_or_default(),
        })
        .into())
    }

    fn err_handler(_event: Event, _ctx: Context) -> Result<Response, Error> {
        Err("boom".into())
    }

    #[test]
    fn enveloped_line_populates_context_and_unwraps_event() {
        let line = r#"{"event":{"rawPath":"/x","requestContext":{"requestId":"r-1"}},"__riz_deadline_ms":42,"__riz_function_name":"fn-a"}"#;
        let out = process_line(line, ok_handler);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["fn"], "fn-a");
        assert_eq!(v["rid"], "r-1");
        assert_eq!(v["deadline"], 42);
        assert_eq!(v["path"], "/x");
    }

    #[test]
    fn bare_event_falls_back() {
        let line = r#"{"rawPath":"/bare"}"#;
        let out = process_line(line, ok_handler);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["fn"], "unknown");
        assert_eq!(v["rid"], "");
        assert_eq!(v["deadline"], 0);
        assert_eq!(v["path"], "/bare");
    }

    #[test]
    fn malformed_line_yields_canonical_400() {
        let out = process_line("not json", ok_handler);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["statusCode"], 400);
        assert_eq!(v["body"], r#"{"message":"bad event json"}"#);
        assert_eq!(v["isBase64Encoded"], false);
    }

    #[test]
    fn handler_error_yields_500_and_keeps_shape() {
        let out = process_line(r#"{"rawPath":"/x"}"#, err_handler);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["statusCode"], 500);
        assert!(v["body"].as_str().unwrap().contains("boom"));
        assert!(v["headers"]["content-type"] == "application/json");
    }

    #[test]
    fn remaining_time_clamps_at_zero() {
        let past = Context {
            function_name: "f".into(),
            request_id: String::new(),
            deadline_ms: 1,
        };
        assert_eq!(past.remaining_time(), Duration::ZERO);
        let future = Context {
            function_name: "f".into(),
            request_id: String::new(),
            deadline_ms: now_millis() + 60_000,
        };
        assert!(future.remaining_time() > Duration::from_secs(30));
    }

    #[test]
    fn decode_response_success_rows() {
        let bytes = br#"{"ok":true,"rows":[{"a":1},{"a":2}],"row_count":2}"#;
        let rows = cap::pg::decode_response(bytes).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["a"], 1);
    }

    #[test]
    fn decode_response_error_envelope() {
        let bytes = br#"{"ok":false,"error":{"code":"denied","message":"no grant"}}"#;
        let err = cap::pg::decode_response(bytes).unwrap_err();
        assert_eq!(err.code, "denied");
        assert_eq!(err.message, "no grant");
    }

    #[test]
    fn decode_response_garbage_is_local_bad_request() {
        let err = cap::pg::decode_response(b"\xff\xfe not json").unwrap_err();
        assert_eq!(err.code, "bad_request");
    }

    #[test]
    fn host_query_fails_closed_with_denied() {
        let err = cap::pg::query("db", "select 1", &[]).unwrap_err();
        assert_eq!(err.code, "denied");
    }
}
