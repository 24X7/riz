//! The `riz __telemetry <sink-path>` worker entry.
//!
//! Synchronous on purpose — like the WASM host (`process::wasm::run_host`), it
//! runs *before* any tokio runtime is constructed (see `main()`), keeping the
//! child lean and fully isolated from the host event loop.
//!
//! 2a behaviour: read length-prefixed [`TelemetryEvent`] frames from stdin in a
//! blocking loop and append each one as a JSON line to the sink file. This is
//! the seam where phase 2b drops in the real OTLP/HTTP-JSON exporter.

use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, Write};

use anyhow::Context;

use crate::observability::ipc::read_frame;

/// Entry point for `riz __telemetry <sink>`. `args` is everything after
/// `__telemetry` (i.e. `argv[2..]`); `args[0]` is the sink file path.
pub fn run_worker(args: &[String]) -> anyhow::Result<()> {
    let sink_path = args
        .first()
        .context("__telemetry: missing <sink-path> argument")?;

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
                let line = serde_json::to_string(&ev)
                    .context("__telemetry: serialize event")?;
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
