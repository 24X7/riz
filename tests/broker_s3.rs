//! Broker `s3.*` against a mock S3 endpoint that INDEPENDENTLY re-signs each
//! received request and BYTE-COMPARES the signature to the one the daemon sent.
//! If our canonicalization is wrong the signatures diverge and the test fails —
//! the AWS-test-vector-equivalent correctness proof for S3 SigV4, end to end
//! through the broker dispatcher. No network, no real AWS.
//!
//! Also proves: the object key rides the URL path, `x-amz-content-sha256` is a
//! signed header, read-only op restriction (a mutation is rejected before it is
//! ever signed/sent), and `key_prefix` scoping.

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    sign, PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use riz::broker::s3::S3Backend;
use riz::broker::{Broker, GrantBackend};
use riz::config::{CapabilityGrant, S3ResourceConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AK: &str = "AKIDEXAMPLE";
const SK: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const OBJECT_BODY: &str = "hello from the sandbox";

#[derive(Default)]
struct MockState {
    /// Per request: (method, request-target path, signature-ok).
    seen: Mutex<Vec<(String, String, bool)>>,
    hits: AtomicUsize,
}

/// A mock S3 endpoint. For each request it recomputes the SigV4 signature from
/// the received method/uri/host/body and asserts it matches the `Signature=` in
/// the Authorization header, then answers a canned object body.
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
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
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
            let uri = format!("http://{host}{path}");
            let ok = verify_signature(&method, &uri, &headers, &body);
            st.seen.lock().unwrap().push((method, path, ok));

            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                OBJECT_BODY.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(OBJECT_BODY.as_bytes());
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
/// `Signature=` in the Authorization header. The daemon signed only `host`;
/// `sign()` then added `x-amz-date` and (with the payload-checksum setting)
/// `x-amz-content-sha256`. Reproduce with the SAME inputs, time, and body.
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
    let host = header(headers, "host").unwrap_or_default();
    let signed_headers: Vec<(&str, &str)> = vec![("host", host.as_str())];

    let identity: Identity = Credentials::new(AK, SK, None, None, "test").into();
    let mut settings = SigningSettings::default();
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region("us-east-1")
        .name("s3")
        .time(time)
        .settings(settings)
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

fn s3_grant(mode: &str, key_prefix: Option<&str>) -> CapabilityGrant {
    let mut g: CapabilityGrant = toml::from_str(
        r#"
type = "s3"
resource = "s3.main"
"#,
    )
    .unwrap();
    g.mode = mode.to_string();
    g.key_prefix = key_prefix.map(|s| s.to_string());
    g
}

fn broker_for(port: u16, grant: CapabilityGrant) -> Broker {
    std::env::set_var("RIZ_S3_E2E_AK", AK);
    std::env::set_var("RIZ_S3_E2E_SK", SK);
    let res: S3ResourceConfig = toml::from_str(&format!(
        r#"
region = "us-east-1"
bucket = "widgets"
endpoint_url = "http://127.0.0.1:{port}"
access_key_id_env = "RIZ_S3_E2E_AK"
secret_access_key_env = "RIZ_S3_E2E_SK"
"#
    ))
    .unwrap();
    let backend = Arc::new(S3Backend::from_resource(&res).expect("build s3 backend"));
    let mut grants = indexmap::IndexMap::new();
    grants.insert("bucket".to_string(), grant);
    let mut backends = std::collections::HashMap::new();
    backends.insert("bucket".to_string(), GrantBackend::S3(backend));
    Broker::from_backends(&grants, backends)
}

async fn call(broker: &Broker, verb: &str, req: serde_json::Value) -> serde_json::Value {
    let bytes = broker
        .dispatch(verb, "bucket", req.to_string().as_bytes())
        .await;
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_object_signs_correctly_and_returns_the_body() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, s3_grant("read-write", None));

    let out = call(
        &broker,
        "s3.get_object",
        serde_json::json!({ "key": "tenant-42/report.json" }),
    )
    .await;
    assert_eq!(out["ok"], true, "get_object should succeed: {out}");
    assert_eq!(
        out["rows"][0]["body"], OBJECT_BODY,
        "returns the object body: {out}"
    );

    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one request reached S3");
    assert_eq!(seen[0].0, "GET", "get_object is a GET");
    assert_eq!(
        seen[0].1, "/widgets/tenant-42/report.json",
        "the key rides the URL path under the bucket"
    );
    assert!(
        seen[0].2,
        "SigV4 signature recomputed by the mock must byte-match"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_object_signs_correctly_with_a_body() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, s3_grant("read-write", None));

    let out = call(
        &broker,
        "s3.put_object",
        serde_json::json!({ "key": "tenant-42/new.txt", "body": "some content" }),
    )
    .await;
    assert_eq!(out["ok"], true, "put_object should succeed: {out}");

    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "PUT", "put_object is a PUT");
    assert!(
        seen[0].2,
        "signature over the body (payload hash) must byte-match"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_grant_rejects_a_put_before_signing() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, s3_grant("read-only", None));

    let out = call(
        &broker,
        "s3.put_object",
        serde_json::json!({ "key": "tenant-42/x", "body": "nope" }),
    )
    .await;
    assert_eq!(out["ok"], false, "read-only must reject put_object: {out}");
    assert_eq!(out["error"]["code"], "backend");
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        0,
        "a denied mutation must never be signed or sent to S3"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_prefix_grant_rejects_an_off_prefix_key() {
    let (port, mock) = start_mock();
    let broker = broker_for(port, s3_grant("read-write", Some("tenant-42/")));

    // In-prefix → allowed.
    let ok = call(
        &broker,
        "s3.get_object",
        serde_json::json!({ "key": "tenant-42/report.json" }),
    )
    .await;
    assert_eq!(ok["ok"], true, "in-prefix key allowed: {ok}");

    // Off-prefix → rejected before signing.
    let bad = call(
        &broker,
        "s3.get_object",
        serde_json::json!({ "key": "tenant-99/report.json" }),
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
