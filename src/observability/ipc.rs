//! IPC wire format between the host and the isolated `__telemetry` child.
//!
//! Events are length-prefixed JSON: a `u32`-LE byte length followed by exactly
//! that many `serde_json` bytes. Length-prefixing (rather than newline
//! delimiting) keeps the framing binary-safe and trivially resyncable, and lets
//! the reader recover cleanly from partial reads and EOF.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// OpenTelemetry span kind. Mapped to the request → function → chat-completion
/// tree in phase 2d: request = `Server`, function = `Internal`, LLM call =
/// `Client`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Server,
    Internal,
    Client,
}

/// OTel GenAI semantic-convention attribute keys. These are just well-known
/// string keys in the generic [`TelemetryEvent::attributes`] map — naming them
/// here keeps the request → chat-completion wiring and the OTLP encoder honest
/// about which conventions we follow.
pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
/// `gen_ai.operation.name` — the GenAI operation ("chat" for chat completions).
/// Current OTel GenAI semconv; lets Datadog LLM Observability and others classify
/// the span. `gen_ai.provider.name` is the current name for what older semconv
/// called `gen_ai.system`; we emit both so old and new backends light up.
pub const GEN_AI_OPERATION: &str = "gen_ai.operation.name";
pub const GEN_AI_PROVIDER: &str = "gen_ai.provider.name";

/// A typed span attribute value (OTel `AnyValue` subset we use).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrValue {
    String(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

/// A single span event sent from host to telemetry child. In 2a the child
/// appends these verbatim (as JSON lines) to its sink; in 2b they are batched
/// and exported as OTLP/HTTP-JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub name: String,
    pub kind: SpanKind,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_unix_nanos: u64,
    pub end_unix_nanos: u64,
    #[serde(default)]
    pub attributes: BTreeMap<String, AttrValue>,
}

/// Write a single event as a `u32`-LE length prefix + JSON body.
pub fn write_frame<W: Write>(w: &mut W, ev: &TelemetryEvent) -> io::Result<()> {
    let body = serde_json::to_vec(ev).map_err(io::Error::other)?;
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    Ok(())
}

/// Read a single length-prefixed event. Returns `Ok(None)` on a clean EOF at a
/// frame boundary (the writer closed the pipe). Partial frames after the prefix
/// are read to completion via `read_exact`.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<TelemetryEvent>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        // Clean EOF exactly at a frame boundary.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let ev = serde_json::from_slice(&body).map_err(io::Error::other)?;
    Ok(Some(ev))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TelemetryEvent {
        let mut attributes = BTreeMap::new();
        attributes.insert("gen_ai.system".to_string(), AttrValue::String("riz".into()));
        attributes.insert("gen_ai.usage.input_tokens".to_string(), AttrValue::Int(42));
        TelemetryEvent {
            name: "chat-completion".into(),
            kind: SpanKind::Client,
            trace_id: "t".into(),
            span_id: "s".into(),
            parent_span_id: Some("p".into()),
            start_unix_nanos: 10,
            end_unix_nanos: 20,
            attributes,
        }
    }

    #[test]
    fn frame_roundtrips() {
        let ev = sample();
        let mut buf = Vec::new();
        write_frame(&mut buf, &ev).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let got = read_frame(&mut cur).unwrap().unwrap();
        assert_eq!(got, ev);
        // Next read is a clean EOF.
        assert!(read_frame(&mut cur).unwrap().is_none());
    }

    #[test]
    fn multiple_frames_in_stream() {
        let mut buf = Vec::new();
        for i in 0..3 {
            let mut ev = sample();
            ev.name = format!("e{i}");
            write_frame(&mut buf, &ev).unwrap();
        }
        let mut cur = std::io::Cursor::new(buf);
        for i in 0..3 {
            let got = read_frame(&mut cur).unwrap().unwrap();
            assert_eq!(got.name, format!("e{i}"));
        }
        assert!(read_frame(&mut cur).unwrap().is_none());
    }
}
