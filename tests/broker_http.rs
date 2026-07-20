//! Broker `http.fetch` against a local mock origin — proves the http backend
//! end-to-end through the shared dispatcher: origin pinning (relative path
//! under base_url), host-injected auth, the per-grant method allow-list, and
//! the closed error set. No network.
//!
//! The mock binds loopback, so the resource opts into `allow_private_ips`
//! (an operator-declared internal target); SSRF private-IP refusal is the
//! default and is unit-tested in src/broker/http.rs.

use riz::broker::http::HttpBackend;
use riz::broker::{Broker, GrantBackend};
use riz::config::{CapabilityGrant, HttpResourceConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
struct RecordedReq {
    method: String,
    path: String,
    authorization: Option<String>,
}

/// A tiny blocking HTTP/1.1 mock: records method/path/Authorization for each
/// request and answers `200 {"ok":"mock"}`. Runs on its own thread alongside
/// the test's tokio runtime.
fn start_mock() -> (u16, Arc<Mutex<Vec<RecordedReq>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::<RecordedReq>::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req = RecordedReq::default();
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            req.method = parts.next().unwrap_or_default().to_string();
            req.path = parts.next().unwrap_or_default().to_string();
            let mut content_length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                // Match header NAMES case-insensitively but keep the value's
                // case (Authorization values are case-sensitive).
                if let Some((name, value)) = h.split_once(':') {
                    match name.trim().to_ascii_lowercase().as_str() {
                        "authorization" => req.authorization = Some(value.trim().to_string()),
                        "content-length" => content_length = value.trim().parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            if content_length > 0 {
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
            }
            log2.lock().unwrap().push(req);
            let body = br#"{"ok":"mock"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    (port, log)
}

fn http_grant(mode: &str, methods: &[&str]) -> CapabilityGrant {
    let mut g: CapabilityGrant = toml::from_str(
        r#"
type = "http"
resource = "http.api"
"#,
    )
    .unwrap();
    g.mode = mode.to_string();
    g.methods = methods.iter().map(|s| s.to_string()).collect();
    g
}

fn broker_for(port: u16, token_env: &str, grant: CapabilityGrant) -> Broker {
    std::env::set_var(token_env, "sk_test_secret");
    let res: HttpResourceConfig = toml::from_str(&format!(
        r#"
base_url = "http://127.0.0.1:{port}/v1"
allow_private_ips = true
auth = {{ kind = "bearer", token_env = "{token_env}" }}
"#
    ))
    .unwrap();
    let backend = Arc::new(HttpBackend::from_resource(&res).expect("build http backend"));
    let mut grants = indexmap::IndexMap::new();
    grants.insert("api".to_string(), grant);
    let mut backends = std::collections::HashMap::new();
    backends.insert("api".to_string(), GrantBackend::Http(backend));
    Broker::from_backends(&grants, backends)
}

async fn call(broker: &Broker, method: &str, path: &str) -> serde_json::Value {
    let req = serde_json::json!({ "method": method, "path": path })
        .to_string()
        .into_bytes();
    let bytes = broker.dispatch("http.fetch", "api", &req).await;
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_fetch_pins_origin_injects_auth_and_enforces_methods() {
    let (port, log) = start_mock();
    let broker = broker_for(
        port,
        "RIZ_HTTP_E2E_TOKEN_1",
        http_grant("read-write", &["GET", "POST"]),
    );

    // 1. Allowed GET on a relative path → 200; mock saw the path under /v1 and
    //    the host-injected bearer token.
    let ok = broker
        .dispatch(
            "http.fetch",
            "api",
            serde_json::json!({ "method": "GET", "path": "/charges" })
                .to_string()
                .as_bytes(),
        )
        .await;
    let v: serde_json::Value = serde_json::from_slice(&ok).unwrap();
    assert_eq!(v["ok"], true, "granted GET should succeed: {v}");
    assert_eq!(v["rows"][0]["status"], 200, "mock 200; {v}");
    assert!(
        v["rows"][0]["body"].as_str().unwrap().contains("mock"),
        "mock body echoed; {v}"
    );

    let recorded = log.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "one request reached the mock");
    assert_eq!(recorded[0].method, "GET");
    assert_eq!(
        recorded[0].path, "/v1/charges",
        "path joined under base_url path prefix"
    );
    assert_eq!(
        recorded[0].authorization.as_deref(),
        Some("Bearer sk_test_secret"),
        "daemon injected the bearer token host-side"
    );

    // 2. Off-list method → error (never reaches the mock).
    let denied = call(&broker, "DELETE", "/charges/1").await;
    assert_eq!(denied["ok"], false);
    assert_eq!(
        denied["error"]["code"], "backend",
        "method not permitted surfaces as a backend error: {denied}"
    );

    // 3. Absolute URL in path → refused (guest may not name an origin).
    let abs = call(&broker, "GET", "http://169.254.169.254/latest/meta-data").await;
    assert_eq!(abs["ok"], false);
    assert!(
        abs["error"]["message"]
            .as_str()
            .unwrap()
            .contains("relative"),
        "absolute URL rejected: {abs}"
    );

    // Only the one legitimate GET ever reached the origin.
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "denied calls never dial the origin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_http_grant_forces_get() {
    let (port, _log) = start_mock();
    // Grant lists POST, but read-only mode must force GET-only.
    let broker = broker_for(
        port,
        "RIZ_HTTP_E2E_TOKEN_2",
        http_grant("read-only", &["POST"]),
    );
    let denied = call(&broker, "POST", "/charges").await;
    assert_eq!(denied["ok"], false);
    assert_eq!(
        denied["error"]["code"], "backend",
        "read-only forbids POST: {denied}"
    );
}
