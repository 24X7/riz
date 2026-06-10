//! Hand-rolled OTLP/HTTP-JSON trace exporter.
//!
//! No `opentelemetry*` / `tonic` / `prost` crates — the product is one ~10MB
//! Rust binary and we will not bloat it with the OTel crate tree. The encoder
//! is `serde_json` and the transport is `reqwest::blocking` (both already deps).
//!
//! [`encode_resource_spans`] maps a batch of [`TelemetryEvent`]s to the OTLP
//! trace JSON shape accepted at `POST /v1/traces`:
//!
//! ```json
//! { "resourceSpans": [ {
//!     "resource": { "attributes": [ {"key":"service.name","value":{"stringValue":"riz"}} ] },
//!     "scopeSpans": [ {
//!       "scope": { "name": "riz" },
//!       "spans": [ { "traceId":"…","spanId":"…","parentSpanId":"…","name":"…",
//!                    "kind":1,"startTimeUnixNano":"…","endTimeUnixNano":"…",
//!                    "attributes":[ {"key":"…","value":{"intValue":"…"}} ] } ]
//!     } ]
//! } ] }
//! ```
//!
//! There is exactly ONE export path. Datadog, CloudWatch/X-Ray, Honeycomb, and
//! any OTLP collector are all just a different `endpoint` + `headers` on this
//! same POST — e.g. Datadog's OTLP intake with a `dd-api-key` header, or an
//! AWS OTel/ADOT collector that forwards to X-Ray. The host never speaks
//! StatsD or any vendor wire protocol directly.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::ipc::{AttrValue, SpanKind, TelemetryEvent};

/// OTLP `SpanKind` enum values (from the trace proto). We map our three kinds.
fn span_kind_code(kind: SpanKind) -> i32 {
    match kind {
        SpanKind::Internal => 1, // SPAN_KIND_INTERNAL
        SpanKind::Server => 2,   // SPAN_KIND_SERVER
        SpanKind::Client => 3,   // SPAN_KIND_CLIENT
    }
}

/// Encode a typed attribute value into the OTLP `AnyValue` wrapper. Ints are
/// stringified per the OTLP/JSON mapping for 64-bit integers.
fn encode_attr_value(v: &AttrValue) -> Value {
    match v {
        AttrValue::String(s) => json!({ "stringValue": s }),
        AttrValue::Int(i) => json!({ "intValue": i.to_string() }),
        AttrValue::Double(d) => json!({ "doubleValue": d }),
        AttrValue::Bool(b) => json!({ "boolValue": b }),
    }
}

/// Encode a `{key -> AttrValue}` map into the OTLP `[{key,value}]` list.
fn encode_attrs(attrs: &BTreeMap<String, AttrValue>) -> Value {
    Value::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": encode_attr_value(v) }))
            .collect(),
    )
}

/// Encode one span event into the OTLP span JSON object.
fn encode_span(ev: &TelemetryEvent) -> Value {
    let mut span = json!({
        "traceId": ev.trace_id,
        "spanId": ev.span_id,
        "name": ev.name,
        "kind": span_kind_code(ev.kind),
        // 64-bit unix-nanos are stringified per the OTLP/JSON mapping.
        "startTimeUnixNano": ev.start_unix_nanos.to_string(),
        "endTimeUnixNano": ev.end_unix_nanos.to_string(),
        "attributes": encode_attrs(&ev.attributes),
    });
    if let Some(parent) = &ev.parent_span_id {
        span["parentSpanId"] = Value::String(parent.clone());
    }
    span
}

/// Encode a batch of events into a single OTLP `resourceSpans` document.
/// All spans go under one resource (`service.name = riz`) and one scope
/// (`name = riz`); span linkage is carried by each span's trace/span/parent ids.
pub fn encode_resource_spans(events: &[TelemetryEvent]) -> Value {
    let spans: Vec<Value> = events.iter().map(encode_span).collect();
    json!({
        "resourceSpans": [ {
            "resource": {
                "attributes": [
                    { "key": "service.name", "value": { "stringValue": "riz" } }
                ]
            },
            "scopeSpans": [ {
                "scope": { "name": "riz" },
                "spans": spans
            } ]
        } ]
    })
}

/// POST an OTLP/HTTP-JSON trace document to `<endpoint>/v1/traces`.
///
/// `endpoint` is the collector base (e.g. `http://localhost:4318`); the
/// `/v1/traces` path is appended. `headers` are added verbatim (auth tokens,
/// `dd-api-key`, etc.) alongside the mandatory `Content-Type: application/json`.
/// Runs on `reqwest::blocking` — the `__telemetry` child has no async runtime.
pub fn export(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> anyhow::Result<()> {
    let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.json(body).send()?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("otlp export to {url} failed: HTTP {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_kind_codes_and_stringified_times() {
        let ev = TelemetryEvent {
            name: "s".into(),
            kind: SpanKind::Server,
            trace_id: "t".into(),
            span_id: "sp".into(),
            parent_span_id: None,
            start_unix_nanos: 5,
            end_unix_nanos: 9,
            attributes: BTreeMap::new(),
        };
        let doc = encode_resource_spans(&[ev]);
        let span = &doc["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["kind"], 2);
        assert_eq!(span["startTimeUnixNano"], "5");
        assert_eq!(span["endTimeUnixNano"], "9");
        assert!(span.get("parentSpanId").is_none());
    }
}
