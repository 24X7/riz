//! Deterministic, network-free provider. Echoes the conversation back so the
//! gateway and the OpenAI-compatible endpoint can be exercised in CI, demos,
//! and offline development without any API key or upstream service.

use super::types::{approx_tokens, ChatRequest, ChatResponse};
use super::ProviderError;

#[derive(Debug)]
pub struct MockProvider;

impl MockProvider {
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .or_else(|| req.messages.last());
        let Some(user_msg) = last_user else {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        };
        let reply = format!("[mock:{}] You said: {}", req.model, user_msg.content);
        let prompt_tokens = req
            .messages
            .iter()
            .map(|m| approx_tokens(&m.content))
            .sum::<u32>();
        let completion_tokens = approx_tokens(&reply);
        Ok(ChatResponse::assistant(
            &req.model,
            reply,
            prompt_tokens,
            completion_tokens,
        ))
    }
}
