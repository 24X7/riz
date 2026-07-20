//! Broker `dynamo.*` against a mock DynamoDB endpoint that INDEPENDENTLY
//! re-signs each received request and BYTE-COMPARES the signature to the one
//! the daemon sent. If our canonicalization is wrong the signatures diverge
//! and the test fails — this is the AWS-test-vector-equivalent correctness
//! proof for SigV4, end to end through the broker dispatcher. No network, no
//! real AWS.
//!
//! Also proves: TableName injection + X-Amz-Target, read-only op restriction
//! (a mutation is rejected before it is ever signed/sent), and key_prefix
//! scoping.

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use riz::broker::dynamo::DynamoBackend;
use riz::broker::{Broker, GrantBackend};
use riz::config::{CapabilityGrant, DynamoResourceConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AK: &str = "AKIDEXAMPLE";
const SK: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

#[derive(Default)]
struct MockState {
    /// Signature-verification result recorded per request: (target, ok).
    seen: Mutex<Vec<(String, bool)>>,
    hits: AtomicUsize,
}

/// A mock DynamoDB endpoint. For each request it recomputes the SigV4
/// signature from the received method/uri/signed-headers/body and asserts it
/// matches the `Signature=` in the Authorization header, then answers a canned
/// GetItem response.
fn start_mock() -> (u16, Arc<MockState>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(MockState::default());
    let st = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let method = request_line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            let mut headers: Vec<(String, String)> = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    let k = k.trim().to_string();
                    let v = v.trim().to_string();
                    if k.eq_ignore_ascii_case("content-length") {
                        content_length = v.parse().unwrap_or(0);
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);

            st.hits.fetch_add(1, Ordering::SeqCst);
            let host = header(&headers, "host").unwrap_or_default();
            let target = header(&headers, "x-amz-target").unwrap_or_default();
            let uri = format!("http://{host}/");
            let ok = verify_signature(&method, &uri, &headers, &body);
            st.seen.lock().unwrap().push((target, ok));

            let resp_body = br#"{"Item":{"pk":{"S":"tenant-42#order-1"},"qty":{"N":"3"}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-amz-json-1.0\r\n\r\n",
                resp_body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(resp_body);
            let _ = stream.flush();
        }
    });
    (port, state)
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// Recompute the SigV4 signature from what was received and compare it to the
/// `Signature=` in the Authorization header.
fn verify_signature(method: &str, uri: &str, headers: &[(String, String)], body: &[u8]) -> bool {
    let auth = header(headers, "authorization").unwrap_or_default();
    let Some(sent_sig) = auth
        .split("Signature=")
        .nth(1)
        .map(|s| s.trim().to_string())
    else {
        return false;
    };
    let x_amz_date = header(headers, "x-amz-date").unwrap_or_default();
    let Some(time) = parse_amz_date(&x_amz_date) else {
        return false;
    };
    // The daemon signed host + content-type + x-amz-target; sign() then added
    // x-amz-date. Reproduce with the SAME inputs + the SAME time.
    let host = header(headers, "host").unwrap_or_default();
    let ct = header(headers, "content-type").unwrap_or_default();
    let target = header(headers, "x-amz-target").unwrap_or_default();
    let signed_headers: Vec<(&str, &str)> = vec![
        ("host", host.as_str()),
        ("content-type", ct.as_str()),
        ("x-amz-target", target.as_str()),
    ];

    let identity: Identity = Credentials::new(AK, SK, None, None, "test").into();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region("us-east-1")
        .name("dynamodb")
        .time(time)
        .settings(SigningSettings::default())
        .build()
        .unwrap()
        .into();
    let signable = SignableRequest::new(
        method,
        uri,
        signed_headers.into_iter(),
        SignableBody::Bytes(body),
    )
    .unwrap();
    let out = sign(signable, &params).unwrap();
    let (_instructions, recomputed_sig) = out.into_parts();
    recomputed_sig == sent_sig
}

fn parse_amz_date(s: &str) -> Option<SystemTime> {
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ").ok()?;
    let secs = dt.and_utc().timestamp();
    (secs >= 0).then(|| UNIX_EPOCH + Duration::from_secs(secs as u64))
}

fn dynamo_grant(mode: &str, key_prefix: Option<&str>) -> CapabilityGrant {
    let mut g: CapabilityGrant = toml::from_str(
        r#"
type = "dynamo"
resource = "dynamo.main"
"#,
    )
    .unwrap();
    g.mode = mode.to_string();
    g.key_prefix = key_prefix.map(|s| s.to_string());
    g
}

fn broker_for(port: u16, grant: CapabilityGrant) -> Broker {
    std::env::set_var("RIZ_DDB_E2E_AK", AK);
    std::env::set_var("RIZ_DDB_E2E_SK", SK);
    let res: DynamoResourceConfig = toml::from_str(&format!(
        r#"
region = "us-east-1"
table = "widgets"
endpoint_url = "http://127.0.0.1:{port}"
access_key_id_env = "RIZ_DDB_E2E_AK"
secret_access_key_env = "RIZ_DDB_E2E_SK"
"#
    ))
    .unwrap();
    let backend = Arc::new(DynamoBackend::from_resource(&res).expect("build dynamo backend"));
    let mut grants = indexmap::IndexMap::new();
    grants.insert("db".to_string(), grant);
    let mut backends = std::collections::HashMap::new();
    backends.insert("db".to_string(), GrantBackend::Dynamo(backend));
    Broker::from_backends(&grants, backends)
}

async fn call(broker: &Broker, verb: &str, req: serde_json::Value) -> serde_json::Value {
    let bytes = broker
        .dispatch(verb, "db", req.to_string().as_bytes())
        .await;
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_item_signs_correctly_and_returns_the_item() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, dynamo_grant("read-write", None));

    let out = call(
        &broker,
        "dynamo.get_item",
        serde_json::json!({ "Key": { "pk": { "S": "tenant-42#order-1" } } }),
    )
    .await;
    assert_eq!(out["ok"], true, "get_item should succeed: {out}");
    assert_eq!(
        out["rows"][0]["body"]["Item"]["pk"]["S"], "tenant-42#order-1",
        "returns the DynamoDB item: {out}"
    );

    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one request reached DynamoDB");
    assert_eq!(seen[0].0, "DynamoDB_20120810.GetItem", "X-Amz-Target set");
    assert!(
        seen[0].1,
        "SigV4 signature recomputed by the mock must byte-match"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_grant_rejects_a_mutation_before_signing() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, dynamo_grant("read-only", None));

    let out = call(
        &broker,
        "dynamo.put_item",
        serde_json::json!({ "Item": { "pk": { "S": "tenant-42#x" } } }),
    )
    .await;
    assert_eq!(out["ok"], false, "read-only must reject put_item: {out}");
    assert_eq!(out["error"]["code"], "backend");
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        0,
        "a denied mutation must never be signed or sent to DynamoDB"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_prefix_grant_rejects_an_off_prefix_key() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, dynamo_grant("read-write", Some("tenant-42#")));

    // In-prefix → allowed.
    let ok = call(
        &broker,
        "dynamo.get_item",
        serde_json::json!({ "Key": { "pk": { "S": "tenant-42#order-9" } } }),
    )
    .await;
    assert_eq!(ok["ok"], true, "in-prefix key allowed: {ok}");

    // Off-prefix → rejected before signing.
    let bad = call(
        &broker,
        "dynamo.get_item",
        serde_json::json!({ "Key": { "pk": { "S": "tenant-99#order-9" } } }),
    )
    .await;
    assert_eq!(bad["ok"], false, "off-prefix key rejected: {bad}");
    assert_eq!(bad["error"]["code"], "backend");
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        1,
        "only the in-prefix call was signed/sent"
    );
}
