//! Live OTLP-collector smoke: proves a REAL OpenTelemetry Collector accepts
//! riz's hand-rolled OTLP/HTTP-JSON trace export. Unit tests can prove the JSON
//! shape; only a real collector proves the bytes are accepted on the wire — the
//! foundation for "OTel / Datadog / Honeycomb / Tempo all just work."
//!
//! Gated on `RIZ_OTLP_DOCKER=1` (needs Docker), so the normal hermetic suite
//! skips it. It starts `otel/opentelemetry-collector` with
//! `docs/integrations/otel-collector.yaml`, exports a span through the SAME
//! public exporter the runtime uses (`riz::observability::otel::export`), and
//! asserts the collector logged the span with riz's `service.name` + a valid
//! hex trace id.
//!
//! Run: `RIZ_OTLP_DOCKER=1 cargo nextest run --test telemetry_otlp_collector`

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use riz::observability::ipc::{AttrValue, SpanKind, TelemetryEvent};
use riz::observability::otel::{encode_resource_spans, export};

fn enabled() -> bool {
    std::env::var("RIZ_OTLP_DOCKER").as_deref() == Ok("1")
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn collector_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/integrations/otel-collector.yaml")
}

/// Removes the collector container on drop so a failed assertion never leaks it.
struct Container(String);
impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

#[test]
fn real_collector_accepts_riz_otlp_export() {
    if !enabled() {
        eprintln!("SKIP: set RIZ_OTLP_DOCKER=1 (needs Docker) to run the live OTLP smoke");
        return;
    }
    assert!(docker_available(), "RIZ_OTLP_DOCKER=1 but `docker` is not usable");

    // Start a collector that prints received spans (debug exporter).
    let cfg = collector_config();
    let run = Command::new("docker")
        .args(["run", "-d", "-p", "4318:4318", "-v"])
        .arg(format!("{}:/etc/otelcol/config.yaml", cfg.display()))
        .args([
            "otel/opentelemetry-collector:latest",
            "--config",
            "/etc/otelcol/config.yaml",
        ])
        .output()
        .expect("docker run");
    assert!(
        run.status.success(),
        "failed to start collector: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    let _container = Container(id.clone());

    // A span with production-shaped ids (32-hex trace, 16-hex span).
    let mut attrs = BTreeMap::new();
    attrs.insert("http.route".to_string(), AttrValue::String("/smoke".into()));
    let trace_id = "0af7651916cd43dd8448eb211c80319c".to_string();
    let ev = TelemetryEvent {
        name: "riz-otlp-smoke".to_string(),
        kind: SpanKind::Server,
        trace_id: trace_id.clone(),
        span_id: "b7ad6b7169203331".to_string(),
        parent_span_id: None,
        start_unix_nanos: 1_700_000_000_000_000_000,
        end_unix_nanos: 1_700_000_000_500_000_000,
        attributes: attrs,
    };
    let body = encode_resource_spans(&[ev]);
    let client = reqwest::blocking::Client::new();

    // The collector takes a moment to bind :4318 — retry the export briefly.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_err = None;
    loop {
        match export(&client, "http://localhost:4318", &BTreeMap::new(), &body) {
            Ok(()) => break,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("collector never accepted the OTLP export: {e}\n{}", logs(&id));
                }
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    let _ = last_err;

    // The collector must have RECEIVED and decoded the span: its debug exporter
    // logs the service name and the span name. Poll the logs briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let out = logs(&id);
        if out.contains("riz-otlp-smoke") && out.contains("riz") {
            // Found our span + the service.name=riz resource attribute.
            return;
        }
        assert!(
            Instant::now() < deadline,
            "collector accepted the POST but did not log the span:\n{out}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn logs(id: &str) -> String {
    let out = Command::new("docker").args(["logs", id]).output();
    match out {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("(docker logs failed: {e})"),
    }
}
