//! OpenAI-compatible HTTP surface (`/_riz/v1/*`).
//!
//! Lets every existing OpenAI client — `openai` (Python/JS), LangChain,
//! LlamaIndex, CrewAI, … — talk to riz by changing only its `base_url` to
//! `http://<host>/_riz/v1`. Requests route through the LLM gateway
//! (src/llm/) to the configured providers.
//!
//! Endpoints (mounted in server::build_app when `[gateway]` is enabled):
//!   POST /_riz/v1/chat/completions   (buffered JSON, or SSE with stream:true —
//!                                     true token passthrough for openai/ollama
//!                                     kinds; synthesized chunks for the rest)
//!   GET  /_riz/v1/models

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::stream;
use serde_json::json;

use crate::llm::{
    ChatRequest, ChatResponse, ChatStream, EmbeddingsRequest, Gateway, ProviderError, Usage,
};
use crate::observability::ipc::{
    AttrValue, SpanKind, TelemetryEvent, GEN_AI_INPUT_TOKENS, GEN_AI_OPERATION,
    GEN_AI_OUTPUT_TOKENS, GEN_AI_PROVIDER, GEN_AI_REQUEST_MODEL, GEN_AI_SYSTEM,
};
use crate::observability::TelemetryHandle;
use crate::server::{new_span_id, new_trace_id, now_unix_nanos};

/// `POST /_riz/v1/chat/completions` — OpenAI chat-completions shape.
/// `stream: true` returns an SSE stream of `chat.completion.chunk` events
/// terminated by `data: [DONE]`; otherwise a single JSON `chat.completion`.
///
/// Emits a request root **Server** span and, on a successful gateway call, a
/// `chat.completions` **Client** child span carrying OTel GenAI token
/// attributes (`gen_ai.system`, `gen_ai.request.model`,
/// `gen_ai.usage.input_tokens`/`output_tokens`). The child's `parent_span_id`
/// is the request span — so the gateway call rolls up under the request.
pub async fn chat_completions(
    gw: Arc<Gateway>,
    telemetry: TelemetryHandle,
    riz_state: Arc<crate::state::RizState>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let trace_id = new_trace_id();
    let request_span_id = new_span_id();
    let request_start = now_unix_nanos();
    let requested_model = req.model.clone();

    if req.stream {
        return chat_completions_streaming(
            gw,
            telemetry,
            riz_state,
            req,
            trace_id,
            request_span_id,
            request_start,
        )
        .await;
    }

    let outcome = gw.chat(&req).await;

    // Child span for the gateway/LLM call, parented to the request span. Also
    // feeds the local token read-model (the --dev TUI) — same data, two sinks.
    if let Ok(resp) = &outcome {
        riz_state.record_tokens(
            &resp.model,
            &gw.resolved_provider(&requested_model),
            resp.usage.prompt_tokens,
            resp.usage.completion_tokens,
        );
        emit_genai_child_span(
            &telemetry,
            &trace_id,
            &request_span_id,
            request_start,
            &resp.model,
            &resp.usage,
        );
    }

    let status: u16 = match &outcome {
        Ok(_) => 200,
        Err(ProviderError::BadRequest(_)) => 400,
        Err(ProviderError::BudgetExceeded) => 412,
        Err(_) => 502,
    };
    emit_request_root_span(
        &telemetry,
        trace_id,
        request_span_id,
        request_start,
        status,
        requested_model,
    );

    match outcome {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => provider_error_response(e),
    }
}

/// The `stream: true` path. Native-SSE providers (openai/ollama kinds) are
/// proxied byte-for-byte — time-to-first-token is the upstream's. Providers
/// without native streaming return buffered and are re-emitted as synthesized
/// chunks (same SSE contract). Token accounting and the GenAI child span fire
/// when usage is known: immediately for buffered, at stream end for proxied.
#[allow(clippy::too_many_arguments)]
async fn chat_completions_streaming(
    gw: Arc<Gateway>,
    telemetry: TelemetryHandle,
    riz_state: Arc<crate::state::RizState>,
    req: ChatRequest,
    trace_id: String,
    request_span_id: String,
    request_start: u64,
) -> Response {
    let requested_model = req.model.clone();
    let provider = gw.resolved_provider(&req.model);
    let on_complete = {
        let telemetry = telemetry.clone();
        let trace_id = trace_id.clone();
        let request_span_id = request_span_id.clone();
        let model = req.model.clone();
        move |usage: Usage| {
            riz_state.record_tokens(
                &model,
                &provider,
                usage.prompt_tokens,
                usage.completion_tokens,
            );
            emit_genai_child_span(
                &telemetry,
                &trace_id,
                &request_span_id,
                request_start,
                &model,
                &usage,
            );
        }
    };

    let outcome = gw.chat_stream(&req, on_complete).await;
    let status: u16 = match &outcome {
        Ok(_) => 200,
        Err(ProviderError::BadRequest(_)) => 400,
        Err(ProviderError::BudgetExceeded) => 412,
        Err(_) => 502,
    };
    // For a proxied stream the root span closes when headers go out — the
    // GenAI child span (emitted at stream end) carries the full duration; a
    // collector links them by id regardless of arrival order.
    emit_request_root_span(
        &telemetry,
        trace_id,
        request_span_id,
        request_start,
        status,
        requested_model,
    );

    match outcome {
        Ok(ChatStream::Upstream(stream)) => (
            [
                (axum::http::header::CONTENT_TYPE, "text/event-stream"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
            ],
            axum::body::Body::from_stream(stream),
        )
            .into_response(),
        Ok(ChatStream::Buffered(resp)) => stream_response(resp).into_response(),
        Err(e) => provider_error_response(e),
    }
}

fn provider_error_response(e: ProviderError) -> Response {
    match e {
        e @ ProviderError::BadRequest(_) => openai_error(StatusCode::BAD_REQUEST, &e.to_string()),
        e @ ProviderError::BudgetExceeded => {
            openai_error(StatusCode::PRECONDITION_FAILED, &e.to_string())
        }
        e => openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// The `chat.completions` **Client** child span carrying OTel GenAI token
/// attributes. `gen_ai.operation.name` (current semconv) + both the legacy
/// `gen_ai.system` and current `gen_ai.provider.name` so old and new
/// OTel-GenAI consumers (e.g. Datadog LLM Observability) classify the span.
fn emit_genai_child_span(
    telemetry: &TelemetryHandle,
    trace_id: &str,
    parent_span_id: &str,
    start_unix_nanos: u64,
    model: &str,
    usage: &Usage,
) {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        GEN_AI_OPERATION.to_string(),
        AttrValue::String("chat".to_string()),
    );
    attrs.insert(
        GEN_AI_SYSTEM.to_string(),
        AttrValue::String("riz-gateway".to_string()),
    );
    attrs.insert(
        GEN_AI_PROVIDER.to_string(),
        AttrValue::String("riz-gateway".to_string()),
    );
    attrs.insert(
        GEN_AI_REQUEST_MODEL.to_string(),
        AttrValue::String(model.to_string()),
    );
    attrs.insert(
        GEN_AI_INPUT_TOKENS.to_string(),
        AttrValue::Int(usage.prompt_tokens as i64),
    );
    attrs.insert(
        GEN_AI_OUTPUT_TOKENS.to_string(),
        AttrValue::Int(usage.completion_tokens as i64),
    );
    telemetry.emit(TelemetryEvent {
        name: "chat.completions".to_string(),
        kind: SpanKind::Client,
        trace_id: trace_id.to_string(),
        span_id: new_span_id(),
        parent_span_id: Some(parent_span_id.to_string()),
        start_unix_nanos,
        end_unix_nanos: now_unix_nanos(),
        attributes: attrs,
    });
}

/// The request root **Server** span. Emitted after the child on the buffered
/// path so a collector sees a complete tree; ids link them regardless of
/// arrival order.
fn emit_request_root_span(
    telemetry: &TelemetryHandle,
    trace_id: String,
    span_id: String,
    start_unix_nanos: u64,
    status: u16,
    requested_model: String,
) {
    let mut req_attrs = BTreeMap::new();
    req_attrs.insert(
        "http.method".to_string(),
        AttrValue::String("POST".to_string()),
    );
    req_attrs.insert(
        "http.route".to_string(),
        AttrValue::String("/_riz/v1/chat/completions".to_string()),
    );
    req_attrs.insert(
        "http.status_code".to_string(),
        AttrValue::Int(status as i64),
    );
    req_attrs.insert(
        GEN_AI_REQUEST_MODEL.to_string(),
        AttrValue::String(requested_model),
    );
    telemetry.emit(TelemetryEvent {
        name: "POST /_riz/v1/chat/completions".to_string(),
        kind: SpanKind::Server,
        trace_id,
        span_id,
        parent_span_id: None,
        start_unix_nanos,
        end_unix_nanos: now_unix_nanos(),
        attributes: req_attrs,
    });
}

/// Re-emit a completed response as an OpenAI streaming chunk sequence. The mock
/// provider has no incremental tokens, so we chunk the finished content — the
/// wire format is real SSE that any streaming client reads correctly. Real
/// providers will proxy upstream token streams when they land.
fn stream_response(
    resp: ChatResponse,
) -> Sse<impl stream::Stream<Item = Result<Event, Infallible>>> {
    let id = resp.id;
    let created = resp.created;
    let model = resp.model;
    let (content, tool_calls, finish) = match resp.choices.into_iter().next() {
        Some(c) if !c.message.tool_calls.is_empty() => {
            (String::new(), c.message.tool_calls, "tool_calls")
        }
        Some(c) => (c.message.content.unwrap_or_default(), Vec::new(), "stop"),
        None => (String::new(), Vec::new(), "stop"),
    };

    let mut events: Vec<Result<Event, Infallible>> = Vec::new();
    // 1. Opening role delta.
    events.push(Ok(chunk_event(
        &id,
        created,
        &model,
        json!({ "role": "assistant" }),
        None,
    )));
    if tool_calls.is_empty() {
        // 2a. Content deltas, preserving spacing so concatenation == the original.
        for (i, word) in content.split(' ').enumerate() {
            let piece = if i == 0 {
                word.to_string()
            } else {
                format!(" {word}")
            };
            if piece.is_empty() {
                continue;
            }
            events.push(Ok(chunk_event(
                &id,
                created,
                &model,
                json!({ "content": piece }),
                None,
            )));
        }
    } else {
        // 2b. Tool-call turn: one delta carrying the indexed tool_calls array —
        // the OpenAI streaming shape clients accumulate by index.
        let calls: Vec<_> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                json!({
                    "index": i,
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.function.name, "arguments": c.function.arguments },
                })
            })
            .collect();
        events.push(Ok(chunk_event(
            &id,
            created,
            &model,
            json!({ "tool_calls": calls }),
            None,
        )));
    }
    // 3. Terminal chunk with finish_reason, then the [DONE] sentinel.
    events.push(Ok(chunk_event(
        &id,
        created,
        &model,
        json!({}),
        Some(finish),
    )));
    events.push(Ok(Event::default().data("[DONE]")));

    Sse::new(stream::iter(events))
}

fn chunk_event(
    id: &str,
    created: i64,
    model: &str,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> Event {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    Event::default().data(chunk.to_string())
}

/// `POST /_riz/v1/embeddings` — OpenAI embeddings shape.
pub async fn embeddings(gw: Arc<Gateway>, Json(req): Json<EmbeddingsRequest>) -> Response {
    match gw.embed(req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e @ ProviderError::BadRequest(_)) => {
            openai_error(StatusCode::BAD_REQUEST, &e.to_string())
        }
        Err(e @ ProviderError::BudgetExceeded) => {
            openai_error(StatusCode::PRECONDITION_FAILED, &e.to_string())
        }
        Err(e) => openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// `GET /_riz/v1/usage` — cumulative cost + token telemetry (AI-FinOps).
pub async fn usage(gw: Arc<Gateway>) -> Response {
    let (budget, total, providers) = gw.usage_snapshot();
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    let providers: serde_json::Map<String, serde_json::Value> = providers
        .into_iter()
        .map(|(name, u)| {
            (
                name,
                json!({
                    "requests": u.requests,
                    "tokens_in": u.tokens_in,
                    "tokens_out": u.tokens_out,
                    "cost_usd": round6(u.cost_usd),
                }),
            )
        })
        .collect();
    Json(json!({
        "budget_usd": budget,
        "total_cost_usd": round6(total),
        "budget_remaining_usd": budget.map(|b| round6((b - total).max(0.0))),
        "providers": providers,
    }))
    .into_response()
}

/// `GET /_riz/v1/models` — lists configured providers as model ids. Use a
/// `provider/model` form in requests to target a specific provider.
pub async fn models(gw: Arc<Gateway>) -> Response {
    let data: Vec<_> = gw
        .provider_names()
        .into_iter()
        .map(|name| json!({ "id": name, "object": "model", "owned_by": "riz" }))
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

/// OpenAI-style error envelope so client SDKs surface a sensible error.
fn openai_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
            }
        })),
    )
        .into_response()
}
