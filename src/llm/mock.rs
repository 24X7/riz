//! Deterministic, network-free provider. Echoes the conversation back so the
//! gateway and the OpenAI-compatible endpoint can be exercised in CI, demos,
//! and offline development without any API key or upstream service.

use super::types::{
    approx_tokens, mock_embedding, ChatRequest, ChatResponse, EmbeddingData, EmbeddingsResponse,
    Usage,
};
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

    pub async fn embed(
        &self,
        model: &str,
        inputs: Vec<String>,
    ) -> Result<EmbeddingsResponse, ProviderError> {
        const DIMS: usize = 16;
        let mut prompt_tokens = 0u32;
        let data = inputs
            .iter()
            .enumerate()
            .map(|(i, text)| {
                prompt_tokens += approx_tokens(text);
                EmbeddingData {
                    object: "embedding".into(),
                    embedding: mock_embedding(text, DIMS),
                    index: i as u32,
                }
            })
            .collect();
        Ok(EmbeddingsResponse {
            object: "list".into(),
            data,
            model: model.to_string(),
            usage: Usage {
                prompt_tokens,
                completion_tokens: 0,
                total_tokens: prompt_tokens,
            },
        })
    }
}
