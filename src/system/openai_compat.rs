//! OpenAI-compatible HTTP surface (`/_riz/v1/*`).
//!
//! Lets every existing OpenAI client — `openai` (Python/JS), LangChain,
//! LlamaIndex, CrewAI, … — talk to riz by changing only its `base_url` to
//! `http://<host>/_riz/v1`. Requests route through the LLM gateway
//! (src/llm/) to the configured providers.
//!
//! Endpoints (mounted in server::build_app when `[gateway]` is enabled):
//!   POST /_riz/v1/chat/completions   (non-streaming today; SSE next)
//!   GET  /_riz/v1/models

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

use crate::llm::{ChatRequest, ChatResponse, EmbeddingsRequest, Gateway, ProviderError};

/// `POST /_riz/v1/chat/completions` — OpenAI chat-completions shape.
/// `stream: true` returns an SSE stream of `chat.completion.chunk` events
/// terminated by `data: [DONE]`; otherwise a single JSON `chat.completion`.
pub async fn chat_completions(gw: Arc<Gateway>, Json(req): Json<ChatRequest>) -> Response {
    let streaming = req.stream;
    match gw.chat(&req).await {
        Ok(resp) if streaming => stream_response(resp).into_response(),
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

/// Re-emit a completed response as an OpenAI streaming chunk sequence. The mock
/// provider has no incremental tokens, so we chunk the finished content — the
/// wire format is real SSE that any streaming client reads correctly. Real
/// providers will proxy upstream token streams when they land.
fn stream_response(resp: ChatResponse) -> Sse<impl stream::Stream<Item = Result<Event, Infallible>>> {
    let id = resp.id;
    let created = resp.created;
    let model = resp.model;
    let content = resp
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();

    let mut events: Vec<Result<Event, Infallible>> = Vec::new();
    // 1. Opening role delta.
    events.push(Ok(chunk_event(&id, created, &model, json!({ "role": "assistant" }), None)));
    // 2. Content deltas, preserving spacing so concatenation == the original.
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
    // 3. Terminal chunk with finish_reason, then the [DONE] sentinel.
    events.push(Ok(chunk_event(&id, created, &model, json!({}), Some("stop"))));
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
