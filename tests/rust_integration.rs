//! Integration test for the Rust runtime adapter.
//!
//! Pre-requisite: the `echo-rust` example must be built with
//! `cargo build --release -p echo-rust` before running this test.
//! The test locates the binary via `CARGO_TARGET_DIR` or the default
//! `<workspace-root>/target/release/echo-rust` path.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn echo_rust_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target"));
    target_dir.join("release").join("echo-rust")
}

/// Returns `true` only when the echo-rust binary exists and appears executable.
fn echo_rust_available() -> bool {
    let bin = echo_rust_binary();
    bin.exists() && {
        // Quick sanity: the file must be non-empty (avoids a broken artifact)
        std::fs::metadata(&bin)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    }
}

/// Send a line-JSON envelope to the echo-rust binary and capture the response.
///
/// The binary speaks the riz line-JSON protocol: one JSON line in → one JSON
/// line out. We spawn it, write one envelope, read one line, then close stdin
/// to let it exit cleanly.
fn invoke_echo_rust(envelope_json: &str) -> Result<serde_json::Value, String> {
    let bin = echo_rust_binary();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", bin.display()))?;

    // Write the envelope and close stdin so the binary's EOF loop exits.
    {
        let stdin = child.stdin.as_mut().ok_or("child has no stdin")?;
        stdin
            .write_all((envelope_json.to_string() + "\n").as_bytes())
            .map_err(|e| format!("write error: {e}"))?;
    }
    // Drop stdin by moving it out of the option — this closes the pipe.
    child.stdin.take();

    // Give the process up to 10 s to respond.
    let output = {
        let (tx, rx) = std::sync::mpsc::channel();
        let child_id = child.id();
        let _ = child_id; // used below if needed
        std::thread::spawn(move || {
            let out = child.wait_with_output();
            let _ = tx.send(out);
        });
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "echo-rust did not respond within 10s".to_string())?
            .map_err(|e| format!("wait_with_output error: {e}"))?
    };

    let stdout_str =
        std::str::from_utf8(&output.stdout).map_err(|e| format!("non-utf8 stdout: {e}"))?;
    let first_line = stdout_str
        .lines()
        .next()
        .ok_or("echo-rust produced no output")?;

    serde_json::from_str(first_line)
        .map_err(|e| format!("response is not valid JSON: {e}\nraw: {first_line}"))
}

#[test]
fn rust_runtime_echo_responds_200() {
    if !echo_rust_available() {
        // Build not present — skip gracefully rather than failing CI that
        // hasn't run `cargo build --release -p echo-rust` first.
        eprintln!(
            "SKIP: echo-rust binary not found at {}. \
             Run `cargo build --release -p echo-rust` first.",
            echo_rust_binary().display()
        );
        return;
    }

    // Minimal API-GW v2 HTTP request envelope as riz would send it.
    let envelope = serde_json::json!({
        "event": {
            "version": "2.0",
            "routeKey": "GET /echo",
            "rawPath": "/echo",
            "rawQueryString": "",
            "headers": { "host": "localhost" },
            "requestContext": {
                "accountId": "000000000000",
                "apiId": "test",
                "domainName": "localhost",
                "domainPrefix": "localhost",
                "http": {
                    "method": "GET",
                    "path": "/echo",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "127.0.0.1",
                    "userAgent": "test"
                },
                "requestId": "req-1",
                "routeKey": "GET /echo",
                "stage": "$default",
                "time": "01/Jan/2026:00:00:00 +0000",
                "timeEpoch": 1767225600000_i64
            },
            "isBase64Encoded": false
        },
        "__riz_deadline_ms": 9_999_999_999_i64,
        "__riz_function_name": "echo"
    });

    let resp =
        invoke_echo_rust(&serde_json::to_string(&envelope).unwrap()).expect("invocation must work");

    assert_eq!(
        resp["statusCode"], 200,
        "echo-rust must return statusCode 200, got: {resp}"
    );

    // Parse the body and check the echoed path.
    let body_str = resp["body"].as_str().expect("body must be a string");
    let body: serde_json::Value = serde_json::from_str(body_str).expect("body must be valid JSON");

    assert_eq!(
        body["echo"], "/echo",
        "body.echo must reflect rawPath, got: {body}"
    );
    assert_eq!(
        body["method"], "GET",
        "body.method must be GET, got: {body}"
    );
    assert_eq!(
        body["functionName"], "echo",
        "body.functionName must be 'echo', got: {body}"
    );
}

#[test]
fn rust_runtime_config_accepts_rust_runtime() {
    // Verify Config::validate now accepts runtime = "rust" (Wave 6 gate A).
    let toml_str = r#"
[function.echo]
runtime = "rust"
handler = "./target/release/my-handler"

[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("rust runtime must be accepted after Wave 6");
}
