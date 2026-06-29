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
//! same POST — the host never speaks StatsD or any vendor wire protocol
//! directly. The GA route to a vendor is an OTLP collector/agent:
//!   - Datadog: the Datadog Agent (or an OTel Collector + datadog exporter)
//!     OTLP receiver on `:4318`. (Datadog's *agentless* `dd-api-key` OTLP intake
//!     is GA for metrics/logs and Preview for traces.)
//!   - AWS X-Ray: an ADOT / OTel Collector that forwards to X-Ray.
//!   - Honeycomb: `endpoint = https://api.honeycomb.io` + `x-honeycomb-team`.
//!
//! See `docs/integrations/observability.md` for copy-paste `[telemetry]` blocks.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::ipc::{AttrValue, SpanKind, TelemetryEvent};

/// Bounded export retry: up to this many total attempts.
const EXPORT_MAX_ATTEMPTS: usize = 3;
/// Exponential backoff between attempts: 50ms -> 200ms -> 800ms (x4 each step).
const EXPORT_BACKOFF_BASE: Duration = Duration::from_millis(50);
const EXPORT_BACKOFF_FACTOR: u32 = 4;

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

/// The outcome of one export attempt, classified for retry.
enum AttemptError {
    /// Worth retrying within the bounded budget (connection/timeout error, or a
    /// retryable HTTP status: 408, 429, 500, 502, 503, 504).
    Transient(anyhow::Error),
    /// Not worth retrying (e.g. a 4xx other than 408/429, or a malformed URL).
    Permanent(anyhow::Error),
}

/// HTTP status codes we treat as transient (worth a bounded retry).
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
}

/// POST the OTLP document once. 2xx => Ok; retryable status or a
/// connection/timeout reqwest error => `Transient`; any other failure (4xx
/// except 408/429, etc.) => `Permanent`.
fn export_once(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<(), AttemptError> {
    let mut req = client.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = match req.json(body).send() {
        Ok(r) => r,
        Err(e) => {
            // Connection refused, timeout, DNS, etc. — all worth a bounded retry.
            return Err(AttemptError::Transient(anyhow::anyhow!(
                "otlp export to {url}: request error: {e}"
            )));
        }
    };
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let err = anyhow::anyhow!("otlp export to {url} failed: HTTP {status}");
    if is_retryable_status(status) {
        Err(AttemptError::Transient(err))
    } else {
        Err(AttemptError::Permanent(err))
    }
}

/// POST an OTLP/HTTP-JSON trace document to `<endpoint>/v1/traces`, with a
/// bounded exponential-backoff retry on transient failures.
///
/// `endpoint` is the collector base (e.g. `http://localhost:4318`); the
/// `/v1/traces` path is appended. `headers` are added verbatim (auth tokens,
/// `dd-api-key`, etc.) alongside the mandatory `Content-Type: application/json`.
/// Runs on `reqwest::blocking` — the `__telemetry` child has no async runtime.
///
/// Retry policy: up to [`EXPORT_MAX_ATTEMPTS`] (3) attempts; sleep 50ms, 200ms,
/// 800ms between them. Transient = reqwest connection/timeout errors and HTTP
/// 408/429/500/502/503/504. 2xx is success; other 4xx are permanent (no retry).
/// After exhausting the budget the error is returned (the caller logs + drops):
/// telemetry stays best-effort and must never wedge the child.
pub fn export(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> anyhow::Result<()> {
    let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
    let mut backoff = EXPORT_BACKOFF_BASE;
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..EXPORT_MAX_ATTEMPTS {
        match export_once(client, &url, headers, body) {
            Ok(()) => return Ok(()),
            Err(AttemptError::Permanent(e)) => return Err(e),
            Err(AttemptError::Transient(e)) => {
                last_err = Some(e);
                // Don't sleep after the final attempt.
                if attempt + 1 < EXPORT_MAX_ATTEMPTS {
                    std::thread::sleep(backoff);
                    backoff *= EXPORT_BACKOFF_FACTOR;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("otlp export to {url}: exhausted retries")))
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
