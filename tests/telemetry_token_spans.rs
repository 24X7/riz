//! Phase 2b proof: token-aware span tree attribution.
//!
//! A request root span fans out to one or more `chat.completions` child spans,
//! each carrying GenAI token usage attributes. This proves the rollup
//! mechanism: summing the children's `gen_ai.usage.*` under a given request
//! span yields the request's total token usage. Operates on the span model
//! directly — deterministic, no server boot.

use std::collections::BTreeMap;

use riz::observability::ipc::{
    AttrValue, SpanKind, TelemetryEvent, GEN_AI_INPUT_TOKENS, GEN_AI_OUTPUT_TOKENS,
    GEN_AI_REQUEST_MODEL, GEN_AI_SYSTEM,
};

fn request_span(trace: &str, span: &str) -> TelemetryEvent {
    TelemetryEvent {
        name: "POST /_riz/v1/chat/completions".into(),
        kind: SpanKind::Server,
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: None,
        start_unix_nanos: 0,
        end_unix_nanos: 100,
        attributes: BTreeMap::new(),
    }
}

fn chat_span(
    trace: &str,
    span: &str,
    parent: &str,
    model: &str,
    input: i64,
    output: i64,
) -> TelemetryEvent {
    let mut attrs = BTreeMap::new();
    attrs.insert(GEN_AI_SYSTEM.into(), AttrValue::String("openai".into()));
    attrs.insert(GEN_AI_REQUEST_MODEL.into(), AttrValue::String(model.into()));
    attrs.insert(GEN_AI_INPUT_TOKENS.into(), AttrValue::Int(input));
    attrs.insert(GEN_AI_OUTPUT_TOKENS.into(), AttrValue::Int(output));
    TelemetryEvent {
        name: "chat.completions".into(),
        kind: SpanKind::Client,
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: Some(parent.into()),
        start_unix_nanos: 10,
        end_unix_nanos: 90,
        attributes: attrs,
    }
}

/// Sum the GenAI token attributes of every span whose `parent_span_id` is the
/// given request span id.
fn rollup_tokens(events: &[TelemetryEvent], request_span_id: &str) -> (i64, i64) {
    let mut input = 0i64;
    let mut output = 0i64;
    for ev in events {
        if ev.parent_span_id.as_deref() != Some(request_span_id) {
            continue;
        }
        if let Some(AttrValue::Int(n)) = ev.attributes.get(GEN_AI_INPUT_TOKENS) {
            input += n;
        }
        if let Some(AttrValue::Int(n)) = ev.attributes.get(GEN_AI_OUTPUT_TOKENS) {
            output += n;
        }
    }
    (input, output)
}

#[test]
fn request_with_gateway_call_rolls_up_token_usage() {
    let trace = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let req = "1111111111111111";

    // One request root span; two chat.completions children (e.g. a retry or a
    // multi-step agent turn) plus an unrelated child under a DIFFERENT request
    // that must NOT be counted.
    let events = vec![
        request_span(trace, req),
        chat_span(trace, "2222222222222222", req, "gpt-4o", 11, 7),
        chat_span(trace, "3333333333333333", req, "gpt-4o", 20, 13),
        // Belongs to a different request — must be excluded from the rollup.
        chat_span(trace, "4444444444444444", "9999999999999999", "gpt-4o", 100, 100),
    ];

    let (input, output) = rollup_tokens(&events, req);
    assert_eq!(input, 31, "input tokens roll up only this request's children");
    assert_eq!(output, 20, "output tokens roll up only this request's children");
}
