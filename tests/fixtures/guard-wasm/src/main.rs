//! guard-wasm — the WASM guard fixture for riz's guard e2e tests.
//!
//! One module serves BOTH directions (the input shape disambiguates):
//!
//! **guard_in** (payload is a request event — has `rawPath`):
//!   - header `x-guard: deny`    → `{"action":"deny","statusCode":451,...}`
//!   - header `x-guard: mutate`  → allow, with `x-guard-mutated: yes` added
//!     to the event headers (the handler must see it)
//!   - header `x-guard: garbage` → emits a non-JSON line (fail-closed proof)
//!   - otherwise                 → allow
//!
//! **guard_out** (payload is a response envelope — has `statusCode`):
//!   - body contains `deny-me`     → deny 451
//!   - body contains an SSN (the literal `123-45-6789`) → allow with the
//!     response body redacted to `***-**-****` — the PII-redaction demo
//!   - otherwise                   → allow
//!
//! Same line-delimited envelope as every handler: read
//! `{event, __riz_deadline_ms, __riz_function_name}`, write one verdict line.

use std::io::{self, BufRead, Write};

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
        let _ = writeln!(stdout, "{}", verdict(&line));
        let _ = stdout.flush();
    }
}

fn verdict(line: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
    let payload = parsed.get("event").cloned().unwrap_or_default();

    if payload.get("rawPath").is_some() {
        guard_in(payload)
    } else if payload.get("statusCode").is_some() {
        guard_out(payload)
    } else {
        // Unknown payload shape: refuse to guess — the host fails closed.
        r#"{"action":"confused"}"#.to_string()
    }
}

fn guard_in(mut event: serde_json::Value) -> String {
    let directive = event
        .get("headers")
        .and_then(|h| h.get("x-guard"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match directive.as_str() {
        "deny" => serde_json::json!({
            "action": "deny",
            "statusCode": 451,
            "body": "{\"reason\":\"guard denied\"}"
        })
        .to_string(),
        "mutate" => {
            if let Some(headers) = event.get_mut("headers").and_then(|h| h.as_object_mut()) {
                headers.insert(
                    "x-guard-mutated".to_string(),
                    serde_json::Value::String("yes".to_string()),
                );
            }
            serde_json::json!({"action": "allow", "event": event}).to_string()
        }
        // Deliberately broken output — the host must fail closed, not allow.
        "garbage" => "this is not a verdict".to_string(),
        _ => r#"{"action":"allow"}"#.to_string(),
    }
}

fn guard_out(mut response: serde_json::Value) -> String {
    let body = response
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    if body.contains("deny-me") {
        return serde_json::json!({
            "action": "deny",
            "statusCode": 451,
            "body": "{\"reason\":\"response blocked\"}"
        })
        .to_string();
    }
    if body.contains("123-45-6789") {
        let redacted = body.replace("123-45-6789", "***-**-****");
        response["body"] = serde_json::Value::String(redacted);
        return serde_json::json!({"action": "allow", "response": response}).to_string();
    }
    r#"{"action":"allow"}"#.to_string()
}
