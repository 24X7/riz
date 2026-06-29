//! Phase 2a proofs: the telemetry process is ISOLATED and the host emitter is
//! bounded + non-blocking. Telemetry being slow or crashed must never add
//! latency to, or fail, the request path.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use riz::observability::ipc::{AttrValue, SpanKind, TelemetryEvent};
use riz::observability::{TelemetryHandle, TelemetrySupervisor};

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

/// Point a spawned telemetry child at the real `riz` binary even under nextest
/// (where current_exe() is the test runner). Mirrors the WASM host override.
fn set_host_bin_override() {
    if std::env::var_os("RIZ_HOST_BIN").is_none() {
        let exe = env!("CARGO_BIN_EXE_riz");
        std::env::set_var("RIZ_HOST_BIN", exe);
    }
}

/// Read all JSON-line events currently in the sink file.
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

/// Poll the sink until `pred` holds or the deadline passes. Returns the events.
fn poll_sink_until(
    path: &std::path::Path,
    timeout: Duration,
    pred: impl Fn(&[TelemetryEvent]) -> bool,
) -> Vec<TelemetryEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        let evs = read_sink(path);
        if pred(&evs) || Instant::now() >= deadline {
            return evs;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Test 1: emit must NEVER block and must drop when the queue is full.
/// The drain is not running here (simulating a stalled/dead consumer), so the
/// channel saturates immediately. Every emit must still return in O(1).
#[test]
fn emit_never_blocks_and_drops_on_overflow() {
    // Tiny capacity, no drain task running.
    let handle = TelemetryHandle::for_test_stalled(8);

    let n = 100_000;
    let start = Instant::now();
    for i in 0..n {
        handle.emit(ev(&format!("e{i}")));
    }
    let elapsed = start.elapsed();

    // 100k non-blocking try_sends must be far under a second.
    assert!(
        elapsed < Duration::from_secs(1),
        "emit blocked: {n} emits took {elapsed:?}"
    );
    // With capacity 8 and no consumer, the vast majority must be dropped.
    assert!(
        handle.dropped() > 0,
        "expected drops on overflow, got {}",
        handle.dropped()
    );
}

/// Test 2: a real supervisor spawns the `__telemetry` child, and emitted events
/// round-trip through the IPC wire format to the sink file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn telemetry_child_roundtrips_events_to_sink() {
    set_host_bin_override();
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("telemetry.jsonl");

    let sup = TelemetrySupervisor::spawn(&sink, 16, riz::observability::ExportTarget::sink_only())
        .expect("spawn supervisor");
    let handle = sup.handle();

    for i in 0..5 {
        handle.emit(ev(&format!("rt{i}")));
    }

    let evs = poll_sink_until(&sink, Duration::from_secs(10), |e| e.len() >= 5);
    assert!(evs.len() >= 5, "sink only got {} events", evs.len());
    let names: Vec<_> = evs.iter().map(|e| e.name.clone()).collect();
    for i in 0..5 {
        assert!(
            names.contains(&format!("rt{i}")),
            "missing rt{i} in {names:?}"
        );
    }
    sup.shutdown().await;
}

/// Test 3: killing the child must not block or kill the host. emit stays
/// non-blocking right after the kill, and the supervisor respawns a new child
/// that receives subsequently-emitted events at the new sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_emit_survives_child_kill_and_supervisor_respawns() {
    set_host_bin_override();
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("telemetry.jsonl");

    let sup = TelemetrySupervisor::spawn(&sink, 16, riz::observability::ExportTarget::sink_only())
        .expect("spawn supervisor");
    let handle = sup.handle();

    // Make sure the first child is alive and serving.
    handle.emit(ev("before-kill"));
    let _ = poll_sink_until(&sink, Duration::from_secs(10), |e| {
        e.iter().any(|x| x.name == "before-kill")
    });

    let pid = sup.child_pid().expect("child has a pid");

    // Kill the child out from under the supervisor.
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();

    // emit must STILL return immediately right after the kill.
    let start = Instant::now();
    for i in 0..1000 {
        handle.emit(ev(&format!("after-kill-{i}")));
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "emit blocked after child kill"
    );

    // The supervisor must respawn and a NEW event must reach the (new) sink.
    // Keep re-emitting the probe while polling so we don't lose it during the
    // respawn window (events emitted before the new child is up are dropped).
    let probe = "post-respawn-probe";
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        handle.emit(ev(probe));
        std::thread::sleep(Duration::from_millis(100));
        if read_sink(&sink).iter().any(|x| x.name == probe) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "supervisor never respawned: probe never reached the new sink"
        );
    }

    sup.shutdown().await;
}
