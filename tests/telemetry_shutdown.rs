//! P0-3 proofs: graceful telemetry shutdown (no span loss) + bounded OTLP
//! export retry/backoff.
//!
//! 1. `shutdown_flushes_all_pending_events_no_loss`: a real supervisor in
//!    sink mode; emit N events (all within queue capacity, none overflow);
//!    call `shutdown()`; the sink must contain ALL N events. Proves the
//!    drain -> close-stdin -> wait-for-child sequence loses nothing.
//! 2. `export_retries_transient_5xx_then_succeeds`: a local HTTP server that
//!    returns 503 then 200; the export path must ultimately succeed (>=2
//!    requests received). Plus a persistent-500 case that gives up without
//!    hanging.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use riz::observability::ipc::{AttrValue, SpanKind, TelemetryEvent};
use riz::observability::otel::{encode_resource_spans, export};
use riz::observability::{ExportTarget, TelemetrySupervisor};

fn ev(name: &str) -> TelemetryEvent {
    TelemetryEvent {
        name: name.to_string(),
        kind: SpanKind::Internal,
        trace_id: "trace-1".to_string(),
        span_id: format!("span-{name}"),
        parent_span_id: None,
        start_unix_nanos: 1,
        end_unix_nanos: 2,
        attributes: {
            let mut m = BTreeMap::new();
            m.insert("k".to_string(), AttrValue::Int(7));
            m
        },
    }
}

/// Point the spawned telemetry child at the real `riz` binary even under
/// nextest (where current_exe() is the test runner). Mirrors the WASM host
/// override.
fn set_host_bin_override() {
    if std::env::var_os("RIZ_HOST_BIN").is_none() {
        let exe = env!("CARGO_BIN_EXE_riz");
        std::env::set_var("RIZ_HOST_BIN", exe);
    }
}

fn read_sink(path: &std::path::Path) -> Vec<TelemetryEvent> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TelemetryEvent>(l).ok())
        .collect()
}

/// Graceful shutdown must flush EVERY successfully-enqueued event to the sink
/// before it returns — no span loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_flushes_all_pending_events_no_loss() {
    set_host_bin_override();
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("telemetry.jsonl");

    // Generous capacity so none of the N emits overflow-drop.
    let n = 50usize;
    let sup = TelemetrySupervisor::spawn(&sink, 1024, ExportTarget::sink_only())
        .expect("spawn supervisor");
    let handle = sup.handle();

    // Wait until the telemetry child is actually spawned before emitting +
    // shutting down, so the bounded flush window isn't consumed by child spawn
    // latency under heavy CPU contention (keeps this test deterministic).
    let ready = std::time::Instant::now();
    while sup.child_pid().is_none() {
        if ready.elapsed() > std::time::Duration::from_secs(10) {
            panic!("telemetry child never spawned within 10s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    for i in 0..n {
        handle.emit(ev(&format!("s{i}")));
    }

    // Shutdown must drain the channel, close the child's stdin, and wait for
    // the child to flush + exit before returning.
    sup.shutdown().await;

    let evs = read_sink(&sink);
    assert_eq!(
        evs.len(),
        n,
        "expected all {n} events flushed on shutdown, got {} ({:?})",
        evs.len(),
        evs.iter().map(|e| e.name.clone()).collect::<Vec<_>>()
    );
    for i in 0..n {
        assert!(
            evs.iter().any(|e| e.name == format!("s{i}")),
            "missing s{i} after shutdown"
        );
    }
}

/// A tiny HTTP server that replies with a scripted sequence of status codes,
/// one per accepted connection, counting requests. Returns the listener addr
/// and a shared counter.
fn scripted_http_server(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}");
    let count = Arc::new(AtomicUsize::new(0));
    let count_thread = count.clone();

    let handle = std::thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = match listener.accept() {
                Ok(c) => c,
                Err(_) => return,
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            // Drain the request (best-effort: read headers + body).
            let mut buf = [0u8; 8192];
            let mut data = Vec::new();
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = pos + 4;
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
            count_thread.fetch_add(1, Ordering::SeqCst);
            let reason = match status {
                200 => "OK",
                500 => "Internal Server Error",
                503 => "Service Unavailable",
                _ => "Status",
            };
            let resp = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });

    (endpoint, count, handle)
}

fn one_event_body() -> serde_json::Value {
    let mut attrs = BTreeMap::new();
    attrs.insert("http.status_code".to_string(), AttrValue::Int(200));
    let event = TelemetryEvent {
        name: "POST /thing".into(),
        kind: SpanKind::Server,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        span_id: "00000000000000aa".into(),
        parent_span_id: None,
        start_unix_nanos: 1_000,
        end_unix_nanos: 2_000,
        attributes: attrs,
    };
    encode_resource_spans(&[event])
}

/// A transient 503 followed by a 200 must be retried and ultimately succeed,
/// with the batch delivered (not dropped).
#[test]
fn export_retries_transient_5xx_then_succeeds() {
    let (endpoint, count, server) = scripted_http_server(vec![503, 200]);
    let body = one_event_body();
    let client = reqwest::blocking::Client::new();
    let headers = BTreeMap::new();

    export(&client, &endpoint, &headers, &body).expect("export ultimately succeeds after retry");

    server.join().expect("server thread");
    assert!(
        count.load(Ordering::SeqCst) >= 2,
        "expected at least 2 requests (1 transient + 1 success), got {}",
        count.load(Ordering::SeqCst)
    );
}

/// A persistent 500 must give up after the bounded number of attempts and
/// return an error WITHOUT hanging the child.
#[test]
fn export_gives_up_on_persistent_5xx_without_hanging() {
    let (endpoint, count, server) = scripted_http_server(vec![500, 500, 500]);
    let body = one_event_body();
    let client = reqwest::blocking::Client::new();
    let headers = BTreeMap::new();

    let start = std::time::Instant::now();
    let result = export(&client, &endpoint, &headers, &body);
    let elapsed = start.elapsed();

    assert!(result.is_err(), "persistent 500 must surface an error");
    // Bounded backoff total is ~50+200+800ms; well under 5s.
    assert!(
        elapsed < Duration::from_secs(5),
        "export hung on persistent failure: {elapsed:?}"
    );
    server.join().expect("server thread");
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "expected exactly 3 bounded attempts, got {}",
        count.load(Ordering::SeqCst)
    );
}
