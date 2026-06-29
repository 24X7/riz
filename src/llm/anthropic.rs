//! Anthropic provider — maps the OpenAI chat-completions shape to/from the
//! Anthropic Messages API (`POST /v1/messages`).
//!
//! Two shape differences vs. OpenAI handled here:
//!   1. The system prompt is a separate top-level `system` field, not a message
//!      with role "system" — so system turns are split out of `messages`.
//!   2. `max_tokens` is required (OpenAI treats it as optional) — defaults to 4096.
//!
//! `temperature` is intentionally NOT forwarded: it returns 400 on the current
//! Opus models (4.7/4.8). Embeddings are unsupported (Anthropic has no embeddings
//! endpoint) and return a clear error.

use serde::Deserialize;

use super::types::{ChatChoice, ChatMessage, ChatRequest, ChatResponse, EmbeddingsResponse, Usage};
use super::ProviderError;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl AnthropicProvider {
    pub fn new(name: String, base_url: String, api_key: Option<String>) -> Self {
        Self {
            name,
            base_url,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let model = strip_prefix(&req.model, &self.name);
        let (system, messages) = split_system(&req.messages);
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = serde_json::json!(system);
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut rb = self
            .client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.header("x-api-key", key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Unavailable(self.name.clone(), e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let txt: String = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            return Err(ProviderError::Upstream(
                self.name.clone(),
                format!("HTTP {status}: {txt}"),
            ));
        }
        let parsed: AnthropicResponse = resp.json().await.map_err(|e| {
            ProviderError::Upstream(self.name.clone(), format!("malformed response: {e}"))
        })?;

        let content = parsed
            .content
            .iter()
            .filter(|b| b.block_type == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        let finish_reason = match parsed.stop_reason.as_deref() {
            Some("max_tokens") => "length",
            Some("tool_use") => "tool_calls",
            _ => "stop",
        }
        .to_string();

        Ok(ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            model: req.model.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content,
                },
                finish_reason,
            }],
            usage: Usage {
                prompt_tokens: parsed.usage.input_tokens,
                completion_tokens: parsed.usage.output_tokens,
                total_tokens: parsed.usage.input_tokens + parsed.usage.output_tokens,
            },
        })
    }

    pub async fn embed(
        &self,
        _model: &str,
        _inputs: Vec<String>,
    ) -> Result<EmbeddingsResponse, ProviderError> {
        Err(ProviderError::Upstream(
            self.name.clone(),
            "Anthropic has no embeddings endpoint; use a dedicated embeddings provider".into(),
        ))
    }
}

/// Strip a leading `"<provider>/"` so `"anthropic/claude-opus-4-8"` forwards as
/// `"claude-opus-4-8"`.
fn strip_prefix(model: &str, name: &str) -> String {
    model
        .strip_prefix(&format!("{name}/"))
        .unwrap_or(model)
        .to_string()
}

/// Split OpenAI-style messages into Anthropic's (system, messages) shape: system
/// turns are concatenated into the top-level system string; user/assistant turns
/// pass through.
fn split_system(messages: &[ChatMessage]) -> (String, Vec<serde_json::Value>) {
    let system = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let turns = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();
    (system, turns)
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type", default)]
    block_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_system_separates_system_from_turns() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "be terse".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "hello".into(),
            },
        ];
        let (system, turns) = split_system(&msgs);
        assert_eq!(system, "be terse");
        assert_eq!(turns.len(), 2, "system turn must be removed from messages");
        assert_eq!(turns[0]["role"], "user");
    }

    async fn spawn(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn req(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
        }
    }

    #[tokio::test]
    async fn maps_messages_response_to_openai_shape() {
        let resp = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "content": [
                {"type": "thinking", "thinking": "..."},
                {"type": "text", "text": "hello from claude"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        });
        let base = spawn(axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move || {
                let r = resp.clone();
                async move { axum::Json(r) }
            }),
        ))
        .await;

        let p = AnthropicProvider::new("anthropic".into(), base, Some("sk-ant".into()));
        let out = p.chat(&req("anthropic/claude-opus-4-8")).await.unwrap();
        assert_eq!(out.choices[0].message.content, "hello from claude");
        assert_eq!(out.choices[0].finish_reason, "stop");
        assert_eq!(out.usage.prompt_tokens, 5);
        assert_eq!(out.usage.completion_tokens, 3);
        assert_eq!(out.usage.total_tokens, 8);
        // model echoes what the client sent (routed form)
        assert_eq!(out.model, "anthropic/claude-opus-4-8");
    }

    #[tokio::test]
    async fn http_error_maps_to_upstream() {
        let base = spawn(axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "bad key") }),
        ))
        .await;
        let p = AnthropicProvider::new("anthropic".into(), base, None);
        let err = p.chat(&req("anthropic/claude-opus-4-8")).await.unwrap_err();
        assert!(matches!(err, ProviderError::Upstream(_, _)), "got {err:?}");
    }

    #[tokio::test]
    async fn embeddings_unsupported() {
        let p = AnthropicProvider::new("anthropic".into(), "http://x".into(), None);
        let err = p.embed("anthropic/x", vec!["a".into()]).await.unwrap_err();
        assert!(matches!(err, ProviderError::Upstream(_, _)));
    }
}
