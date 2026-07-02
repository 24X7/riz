//! Deterministic, network-free provider. Echoes the conversation back so the
//! gateway and the OpenAI-compatible endpoint can be exercised in CI, demos,
//! and offline development without any API key or upstream service.

use super::types::{
    approx_tokens, mock_embedding, ChatRequest, ChatResponse, EmbeddingData, EmbeddingsResponse,
    ToolCall, ToolCallFunction, Usage,
};
use super::ProviderError;

#[derive(Debug)]
pub struct MockProvider;

impl MockProvider {
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let prompt_tokens = req
            .messages
            .iter()
            .map(|m| approx_tokens(m.text_content()))
            .sum::<u32>();

        // Turn 2 of an agent loop: the client executed our tool call and sent
        // the result back — complete the loop with a final text answer.
        if let Some(result) = req.messages.iter().rev().find(|m| m.role == "tool") {
            let reply = format!(
                "[mock:{}] tool result received: {}",
                req.model,
                result.text_content()
            );
            let completion_tokens = approx_tokens(&reply);
            return Ok(ChatResponse::assistant(
                &req.model,
                reply,
                prompt_tokens,
                completion_tokens,
            ));
        }

        // Turn 1 with tools declared: deterministically call one — the forced
        // tool if `tool_choice` pins one, else the first declared.
        if req.wants_tools() {
            let name = req
                .forced_tool()
                .map(str::to_string)
                .unwrap_or_else(|| req.tools[0].function.name.clone());
            let call = ToolCall {
                id: "call_mock0".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name,
                    arguments: "{}".into(),
                },
            };
            return Ok(ChatResponse::tool_call_turn(
                &req.model,
                vec![call],
                prompt_tokens,
                1,
            ));
        }

        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .or_else(|| req.messages.last())
            .expect("non-empty checked above");
        let reply = format!(
            "[mock:{}] You said: {}",
            req.model,
            last_user.text_content()
        );
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
