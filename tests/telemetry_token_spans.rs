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

/// A non-leaf span (an agent turn or a tool invocation) carrying no token
/// attributes itself — it just structures the chain.
fn intermediate_span(trace: &str, span: &str, parent: &str, name: &str) -> TelemetryEvent {
    TelemetryEvent {
        name: name.into(),
        kind: SpanKind::Internal,
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: Some(parent.into()),
        start_unix_nanos: 5,
        end_unix_nanos: 95,
        attributes: BTreeMap::new(),
    }
}

/// Sum GenAI token attributes over the ENTIRE subtree rooted at `root_span_id`
/// (transitive descendants), not just direct children. This is how a request's
/// full token cost across a multi-step tool/agent chain is attributed — the
/// completions can sit several hops below the request (request → agent.turn →
/// tool → chat.completions).
fn rollup_tokens_tree(events: &[TelemetryEvent], root_span_id: &str) -> (i64, i64) {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut children: HashMap<&str, Vec<&TelemetryEvent>> = HashMap::new();
    for ev in events {
        if let Some(p) = ev.parent_span_id.as_deref() {
            children.entry(p).or_default().push(ev);
        }
    }
    let (mut input, mut output) = (0i64, 0i64);
    let mut seen: HashSet<&str> = HashSet::new();
    let mut q: VecDeque<&str> = VecDeque::new();
    q.push_back(root_span_id);
    while let Some(node) = q.pop_front() {
        if !seen.insert(node) {
            continue; // guard against cycles
        }
        if let Some(kids) = children.get(node) {
            for ev in kids {
                if let Some(AttrValue::Int(n)) = ev.attributes.get(GEN_AI_INPUT_TOKENS) {
                    input += n;
                }
                if let Some(AttrValue::Int(n)) = ev.attributes.get(GEN_AI_OUTPUT_TOKENS) {
                    output += n;
                }
                q.push_back(&ev.span_id);
            }
        }
    }
    (input, output)
}

#[test]
fn multi_hop_agent_chain_rolls_up_token_usage_across_the_tree() {
    let trace = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let req = "1000000000000000";
    let agent = "2000000000000000";
    let tool = "3000000000000000";

    // request → agent.turn → {chat (direct), tool → chat (nested), chat (direct)}.
    // The completions sit at depth 2 and 3 below the request — exactly the
    // multi-step agent/tool chain the substrate exists to make attributable.
    let events = vec![
        request_span(trace, req),
        intermediate_span(trace, agent, req, "agent.turn"),
        intermediate_span(trace, tool, agent, "tool.lookup_order"),
        chat_span(trace, "aaaa000000000001", agent, "claude-opus-4-8", 11, 7),
        chat_span(trace, "aaaa000000000002", tool, "claude-opus-4-8", 20, 13),
        chat_span(trace, "aaaa000000000003", agent, "claude-opus-4-8", 5, 3),
        // A different request entirely — must be excluded from this request's cost.
        chat_span(trace, "aaaa000000000004", "9999999999999999", "claude-opus-4-8", 100, 100),
    ];

    let (input, output) = rollup_tokens_tree(&events, req);
    assert_eq!(input, 36, "input tokens roll up across the whole tool/agent chain (11+20+5)");
    assert_eq!(output, 23, "output tokens roll up across the whole tool/agent chain (7+13+3)");

    // Contrast: the flat direct-children rollup sees NO completions directly
    // under the request (they're all nested under agent.turn), proving the tree
    // walk is what makes multi-hop attribution work.
    let (flat_input, flat_output) = rollup_tokens(&events, req);
    assert_eq!(
        (flat_input, flat_output),
        (0, 0),
        "flat rollup misses nested completions — depth-aware attribution is required"
    );
}
