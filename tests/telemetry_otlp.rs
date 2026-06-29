//! Phase 2b proofs: the hand-rolled OTLP/HTTP-JSON exporter.
//!
//! No `opentelemetry*`/`tonic`/`prost` crates — the encoder is `serde_json`
//! and the transport is `reqwest::blocking`. These tests prove (1) the encoder
//! produces a correct OTLP `resourceSpans` document with `service.name=riz`,
//! the right trace/span/parent linkage, and typed GenAI token attributes, and
//! (2) the export path POSTs that document to the configured `/v1/traces`
//! endpoint with `Content-Type: application/json`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use riz::observability::ipc::{AttrValue, SpanKind, TelemetryEvent};
use riz::observability::otel::{encode_resource_spans, export};

fn root_request_span(trace: &str, span: &str) -> TelemetryEvent {
    let mut attrs = BTreeMap::new();
    attrs.insert("http.method".to_string(), AttrValue::String("POST".into()));
    attrs.insert(
        "http.route".to_string(),
        AttrValue::String("/_riz/v1/chat/completions".into()),
    );
    attrs.insert("http.status_code".to_string(), AttrValue::Int(200));
    TelemetryEvent {
        name: "POST /_riz/v1/chat/completions".into(),
        kind: SpanKind::Server,
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: None,
        start_unix_nanos: 1_000,
        end_unix_nanos: 2_000,
        attributes: attrs,
    }
}

fn chat_child_span(trace: &str, span: &str, parent: &str) -> TelemetryEvent {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "gen_ai.system".to_string(),
        AttrValue::String("openai".into()),
    );
    attrs.insert(
        "gen_ai.request.model".to_string(),
        AttrValue::String("gpt-4o".into()),
    );
    attrs.insert("gen_ai.usage.input_tokens".to_string(), AttrValue::Int(11));
    attrs.insert("gen_ai.usage.output_tokens".to_string(), AttrValue::Int(7));
    TelemetryEvent {
        name: "chat.completions".into(),
        kind: SpanKind::Client,
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: Some(parent.into()),
        start_unix_nanos: 1_100,
        end_unix_nanos: 1_900,
        attributes: attrs,
    }
}

/// The registry proof for the `otlp-export` claim.
#[test]
fn otlp_encoder_emits_genai_token_attrs() {
    let trace = "0123456789abcdef0123456789abcdef";
    let root = "00000000000000aa";
    let child = "00000000000000bb";
    let events = vec![
        root_request_span(trace, root),
        chat_child_span(trace, child, root),
    ];

    let doc = encode_resource_spans(&events);

    // resourceSpans[0].resource carries service.name = riz.
    let resource = &doc["resourceSpans"][0]["resource"];
    let res_attrs = resource["attributes"].as_array().expect("resource attrs");
    let svc = res_attrs
        .iter()
        .find(|a| a["key"] == "service.name")
        .expect("service.name attribute present");
    assert_eq!(svc["value"]["stringValue"], "riz");

    // scopeSpans[0].scope.name = riz.
    let scope_spans = &doc["resourceSpans"][0]["scopeSpans"][0];
    assert_eq!(scope_spans["scope"]["name"], "riz");

    let spans = scope_spans["spans"].as_array().expect("spans array");
    assert_eq!(spans.len(), 2, "both spans encoded");

    let root_span = spans
        .iter()
        .find(|s| s["name"] == "POST /_riz/v1/chat/completions")
        .expect("root span present");
    let child_span = spans
        .iter()
        .find(|s| s["name"] == "chat.completions")
        .expect("child span present");

    // Trace/span/parent linkage.
    assert_eq!(root_span["traceId"], trace);
    assert_eq!(root_span["spanId"], root);
    assert!(
        root_span.get("parentSpanId").is_none()
            || root_span["parentSpanId"] == ""
            || root_span["parentSpanId"].is_null(),
        "root span has no parent"
    );
    assert_eq!(child_span["traceId"], trace);
    assert_eq!(child_span["spanId"], child);
    assert_eq!(child_span["parentSpanId"], root);

    // Times stringified unix-nanos.
    assert_eq!(root_span["startTimeUnixNano"], "1000");
    assert_eq!(root_span["endTimeUnixNano"], "2000");

    // GenAI token attributes present with the correct typed values.
    let child_attrs = child_span["attributes"].as_array().expect("child attrs");
    let find = |key: &str| {
        child_attrs
            .iter()
            .find(|a| a["key"] == key)
            .unwrap_or_else(|| panic!("attr {key} present"))
    };
    assert_eq!(find("gen_ai.usage.input_tokens")["value"]["intValue"], "11");
    assert_eq!(find("gen_ai.usage.output_tokens")["value"]["intValue"], "7");
    assert_eq!(find("gen_ai.system")["value"]["stringValue"], "openai");
    assert_eq!(
        find("gen_ai.request.model")["value"]["stringValue"],
        "gpt-4o"
    );
}

/// Stand up a one-shot HTTP receiver and prove the export path POSTs the OTLP
/// document to `/v1/traces` with the JSON content type and a `resourceSpans`
/// body, including configured headers.
#[test]
fn otlp_worker_posts_to_configured_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}");

    // Receiver thread: accept one connection, read the request, capture it,
    // and return a minimal 200 OK.
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 8192];
        let mut data = Vec::new();
        // Read until we have headers + (best-effort) body.
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            // Once we've seen the header terminator and some body, stop.
            if let Some(pos) = find_subslice(&data, b"\r\n\r\n") {
                let header_end = pos + 4;
                // Try to read Content-Length to know when the body is done.
                let head = String::from_utf8_lossy(&data[..header_end]).to_lowercase();
                if let Some(cl) = head
                    .split("content-length:")
                    .nth(1)
                    .and_then(|s| s.split("\r\n").next())
                    .and_then(|s| s.trim().parse::<usize>().ok())
                {
                    if data.len() - header_end >= cl {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.flush();
        String::from_utf8_lossy(&data).to_string()
    });

    let events = vec![root_request_span(
        "0123456789abcdef0123456789abcdef",
        "00000000000000aa",
    )];
    let body = encode_resource_spans(&events);

    let client = reqwest::blocking::Client::new();
    let mut headers = BTreeMap::new();
    headers.insert("x-api-key".to_string(), "secret".to_string());
    export(&client, &endpoint, &headers, &body).expect("export POST succeeds");

    let raw = handle.join().expect("receiver thread");
    let lower = raw.to_lowercase();
    assert!(
        raw.starts_with("POST /v1/traces"),
        "expected POST to /v1/traces, got request line: {:?}",
        raw.lines().next()
    );
    assert!(
        lower.contains("content-type: application/json"),
        "expected application/json content type in:\n{raw}"
    );
    assert!(
        lower.contains("x-api-key: secret"),
        "expected configured header to be sent in:\n{raw}"
    );
    assert!(
        raw.contains("resourceSpans"),
        "expected resourceSpans in the POST body:\n{raw}"
    );
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
