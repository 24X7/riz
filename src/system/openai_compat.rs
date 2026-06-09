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

use std::sync::Arc;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::llm::{ChatRequest, Gateway, ProviderError};

/// `POST /_riz/v1/chat/completions` — OpenAI chat-completions shape.
pub async fn chat_completions(gw: Arc<Gateway>, Json(req): Json<ChatRequest>) -> Response {
    if req.stream {
        // SSE streaming lands in a follow-up commit; be explicit rather than
        // silently returning a non-streamed body a streaming client can't read.
        return openai_error(
            StatusCode::BAD_REQUEST,
            "streaming (stream=true) is not yet supported — set stream=false",
        );
    }
    match gw.chat(&req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e @ ProviderError::BadRequest(_)) => {
            openai_error(StatusCode::BAD_REQUEST, &e.to_string())
        }
        Err(e) => openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
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
