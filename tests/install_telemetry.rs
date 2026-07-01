//! The install-telemetry endpoint (`web/api/install.js`) records each install
//! execution with platform fields + Vercel geo headers and returns 204. It's a
//! zero-dependency Node function; this test runs it through a mock request and
//! asserts the structured event it logs. Skips cleanly if node is absent (CI
//! has it, so there it runs for real).
//!
//! Run: `cargo nextest run --test install_telemetry`

use std::process::Command;

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok()
}

/// Run web/api/install.js against a mock (req, res) and return combined stdout.
fn run_handler(url: &str, headers_js: &str) -> (String, String, bool) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/web/api/install.js");
    let harness = format!(
        r#"
const handler = require({path:?});
let status = null, ended = false;
const res = {{ setHeader() {{}}, get statusCode() {{ return status; }}, set statusCode(v) {{ status = v; }}, end() {{ ended = true; }} }};
const req = {{ url: {url:?}, headers: {headers_js} }};
handler(req, res);
console.log("STATUS=" + status + " ENDED=" + ended);
"#,
        path = path,
        url = url,
        headers_js = headers_js,
    );
    let out = Command::new("node")
        .arg("-e")
        .arg(&harness)
        .output()
        .expect("run node");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (stdout, stderr, out.status.success())
}

fn parse_event(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("riz-install "))
        .unwrap_or_else(|| panic!("no `riz-install` log line in:\n{stdout}"));
    serde_json::from_str(line.trim_start_matches("riz-install ")).expect("event is valid JSON")
}

#[test]
fn install_endpoint_logs_geo_platform_event_and_returns_204() {
    if !node_available() {
        eprintln!("SKIP install_telemetry: node not on PATH");
        return;
    }
    let (stdout, stderr, ok) = run_handler(
        "/api/install?stage=success&os=Darwin&arch=arm64&target=aarch64-apple-darwin&version=v0.1.0",
        r#"{
          "x-vercel-ip-country": "US",
          "x-vercel-ip-country-region": "CA",
          "x-vercel-ip-city": "San%20Francisco",
          "x-vercel-ip-latitude": "37.77",
          "x-vercel-ip-longitude": "-122.42",
          "x-vercel-ip-timezone": "America/Los_Angeles",
          "user-agent": "riz-install/v0.1.0"
        }"#,
    );
    assert!(ok, "node failed: {stderr}");
    assert!(
        stdout.contains("STATUS=204 ENDED=true"),
        "expected a 204 + res.end(); got:\n{stdout}"
    );

    let v = parse_event(&stdout);
    assert_eq!(v["event"], "install");
    assert_eq!(v["stage"], "success");
    // platform (sent by the install script)
    assert_eq!(v["os"], "Darwin");
    assert_eq!(v["arch"], "arm64");
    assert_eq!(v["target"], "aarch64-apple-darwin");
    assert_eq!(v["version"], "v0.1.0");
    // geo (from the x-vercel-ip-* request headers; city URL-decoded)
    assert_eq!(v["country"], "US");
    assert_eq!(v["region"], "CA");
    assert_eq!(v["city"], "San Francisco");
    assert_eq!(v["lat"], "37.77");
    assert_eq!(v["lon"], "-122.42");
    assert_eq!(v["tz"], "America/Los_Angeles");
    assert!(
        v["ts"].as_str().is_some_and(|s| s.contains('T')),
        "ts should be an ISO-8601 timestamp, got {:?}",
        v["ts"]
    );
}

#[test]
fn install_endpoint_is_robust_to_missing_geo_and_params() {
    if !node_available() {
        eprintln!("SKIP install_telemetry: node not on PATH");
        return;
    }
    // No query params, no geo headers — must still return 204 and log nulls,
    // defaulting stage to "start".
    let (stdout, stderr, ok) = run_handler("/api/install", "{}");
    assert!(ok, "node failed: {stderr}");
    assert!(
        stdout.contains("STATUS=204 ENDED=true"),
        "expected 204 even with no data; got:\n{stdout}"
    );
    let v = parse_event(&stdout);
    assert_eq!(v["event"], "install");
    assert_eq!(v["stage"], "start", "stage should default to start");
    assert!(v["os"].is_null());
    assert!(v["country"].is_null());
}
