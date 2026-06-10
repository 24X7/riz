//! The `riz __telemetry <sink-path>` worker entry.
//!
//! Synchronous on purpose — like the WASM host (`process::wasm::run_host`), it
//! runs *before* any tokio runtime is constructed (see `main()`), keeping the
//! child lean and fully isolated from the host event loop. It uses
//! `reqwest::blocking` for OTLP export — no async runtime in the child.
//!
//! Two modes, selected by `RIZ_TELEMETRY_ENDPOINT`:
//!
//! * **export** (endpoint set): batch received [`TelemetryEvent`]s and POST them
//!   as OTLP/HTTP-JSON to `<endpoint>/v1/traces` with `RIZ_TELEMETRY_HEADERS`
//!   (a JSON object) attached. This is the single export path — Datadog,
//!   CloudWatch/X-Ray, Honeycomb, etc. are just different endpoint+headers.
//! * **sink-file** (no endpoint): append each event as a JSON line to the sink
//!   file given on argv. This is the 2a seam and the path the isolation tests
//!   exercise.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, Write};

use anyhow::Context;

use crate::observability::ipc::{read_frame, TelemetryEvent};
use crate::observability::otel;

/// Max events batched before a flush in export mode. The host drains events to
/// the child one at a time, so we also flush whenever the input stalls (EOF on
/// a `read_frame` returns `None`) — a stalled host never strands a partial
/// batch unexported on shutdown.
const BATCH_MAX: usize = 256;

/// Entry point for `riz __telemetry <sink>`. `args` is everything after
/// `__telemetry` (i.e. `argv[2..]`); `args[0]` is the sink file path.
pub fn run_worker(args: &[String]) -> anyhow::Result<()> {
    let sink_path = args
        .first()
        .context("__telemetry: missing <sink-path> argument")?;

    match std::env::var("RIZ_TELEMETRY_ENDPOINT").ok().filter(|s| !s.is_empty()) {
        Some(endpoint) => run_export(&endpoint),
        None => run_sink(sink_path),
    }
}

/// Export mode: batch frames and POST them as OTLP/HTTP-JSON.
fn run_export(endpoint: &str) -> anyhow::Result<()> {
    let headers: BTreeMap<String, String> = std::env::var("RIZ_TELEMETRY_HEADERS")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let client = reqwest::blocking::Client::new();
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    let mut batch: Vec<TelemetryEvent> = Vec::with_capacity(BATCH_MAX);
    loop {
        match read_frame(&mut reader) {
            Ok(Some(ev)) => {
                batch.push(ev);
                if batch.len() >= BATCH_MAX {
                    flush_export(&client, endpoint, &headers, &mut batch);
                }
            }
            // Clean EOF: the host closed stdin. Flush whatever is buffered.
            Ok(None) => {
                flush_export(&client, endpoint, &headers, &mut batch);
                break;
            }
            Err(e) => return Err(e).context("__telemetry: read frame"),
        }
    }
    Ok(())
}

/// Encode + POST the current batch, then clear it. Export failures are logged to
/// stderr and dropped — telemetry is best-effort and must never wedge the child.
fn flush_export(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    batch: &mut Vec<TelemetryEvent>,
) {
    if batch.is_empty() {
        return;
    }
    let body = otel::encode_resource_spans(batch);
    if let Err(e) = otel::export(client, endpoint, headers, &body) {
        eprintln!("__telemetry: export failed: {e}");
    }
    batch.clear();
}

/// Sink-file mode (2a seam): append each event as a JSON line.
fn run_sink(sink_path: &str) -> anyhow::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(sink_path)
        .with_context(|| format!("__telemetry: cannot open sink {sink_path}"))?;
    let mut sink = BufWriter::new(file);

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    loop {
        match read_frame(&mut reader) {
            Ok(Some(ev)) => {
                // Append as a JSON line. Flush per event so the host (and the
                // isolation tests) can observe events promptly; throughput is
                // not the concern in 2a, the seam and isolation are.
                let line =
                    serde_json::to_string(&ev).context("__telemetry: serialize event")?;
                writeln!(sink, "{line}").context("__telemetry: write sink")?;
                sink.flush().context("__telemetry: flush sink")?;
            }
            // Clean EOF: the host closed stdin. Exit cleanly.
            Ok(None) => break,
            Err(e) => return Err(e).context("__telemetry: read frame"),
        }
    }

    sink.flush().ok();
    Ok(())
}
