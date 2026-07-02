//! OpenAI-compatible chat-completion wire types.
//!
//! These mirror OpenAI's `/v1/chat/completions` request/response shape — the
//! de-facto industry standard since 2023 — so any existing OpenAI client
//! (`openai` Python/JS, LangChain, LlamaIndex, …) works against riz by changing
//! only its `base_url`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    // Wire-contract fields forwarded to the real providers (follow-up commits).
    #[allow(dead_code)]
    #[serde(default)]
    pub temperature: Option<f32>,
    #[allow(dead_code)]
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// OpenAI function-calling: `[{"type":"function","function":{name,description,parameters}}]`.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// `"auto"` / `"none"` / `"required"` / `{"type":"function","function":{"name":…}}`.
    /// Kept as raw JSON — providers map it to their own wire shape.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

impl ChatRequest {
    /// True when this turn may answer with tool calls: tools are declared and
    /// the client didn't opt out with `tool_choice: "none"`.
    pub fn wants_tools(&self) -> bool {
        !self.tools.is_empty() && self.tool_choice.as_ref().and_then(|c| c.as_str()) != Some("none")
    }

    /// The function name forced by `tool_choice: {"type":"function","function":{"name":…}}`,
    /// if the client pinned one.
    pub fn forced_tool(&self) -> Option<&str> {
        self.tool_choice
            .as_ref()?
            .get("function")?
            .get("name")?
            .as_str()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    /// `null` on the wire for assistant turns that only carry `tool_calls`.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on `role: "tool"` result messages — which call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// A plain text message with no tool fields.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    /// The message text, treating wire `null` as empty.
    pub fn text_content(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// A declared tool (OpenAI `tools[]` entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type", default = "function_type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments, passed through verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// A model-issued call (assistant `tool_calls[]` entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-*encoded string* per the OpenAI wire format (not an object).
    #[serde(default)]
    pub arguments: String,
}

fn function_type() -> String {
    "function".into()
}

// Deserialize is derived so the real providers can parse upstream OpenAI-shape
// responses directly into these types (unknown upstream fields are ignored).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    #[serde(default = "default_object")]
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Usage,
}

fn default_object() -> String {
    "chat.completion".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    #[serde(default)]
    pub index: u32,
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

impl ChatResponse {
    /// Build a non-streaming `chat.completion` response from an assistant
    /// message + token counts, generating the `id`/`created` envelope fields.
    pub fn assistant(
        model: &str,
        content: String,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> Self {
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".into(),
            created,
            model: model.to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::text("assistant", content),
                finish_reason: "stop".into(),
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        }
    }

    /// Build a `tool_calls` turn: `content: null`, `finish_reason: "tool_calls"`.
    pub fn tool_call_turn(
        model: &str,
        tool_calls: Vec<ToolCall>,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> Self {
        let mut resp = Self::assistant(model, String::new(), prompt_tokens, completion_tokens);
        resp.choices[0].message.content = None;
        resp.choices[0].message.tool_calls = tool_calls;
        resp.choices[0].finish_reason = "tool_calls".into();
        resp
    }
}

/// Rough whitespace-based token estimate. Real providers report exact counts;
/// the mock provider and pre-flight budget checks use this approximation.
pub fn approx_tokens(text: &str) -> u32 {
    text.split_whitespace().count().max(1) as u32
}

// ───────────────────────── Embeddings (OpenAI /v1/embeddings) ─────────────

/// OpenAI's `input` accepts either a single string or an array of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Many(Vec<String>),
}

impl EmbeddingInput {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            EmbeddingInput::Single(s) => vec![s],
            EmbeddingInput::Many(v) => v,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingsRequest {
    pub model: String,
    pub input: EmbeddingInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingsResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    pub object: String,
    pub embedding: Vec<f32>,
    pub index: u32,
}

/// Deterministic mock embedding: an FNV-seeded LCG produces `dims` floats in
/// [-1, 1). Stable for a given input — good enough for demos, CI, and offline
/// similarity tests without any embeddings provider.
pub fn mock_embedding(text: &str, dims: usize) -> Vec<f32> {
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        seed ^= b as u64;
        seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (0..dims)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (seed >> 33) as f32 / (1u64 << 31) as f32; // [0, 1)
            unit * 2.0 - 1.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assistant_message_with_null_content_and_tool_calls() {
        // The exact shape OpenAI returns for a tool-call turn.
        let msg: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {"name": "lookup_order", "arguments": "{\"order_id\":\"42\"}"}
            }]
        }))
        .expect("null content must parse");
        assert_eq!(msg.content, None);
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].function.name, "lookup_order");
    }

    #[test]
    fn plain_message_serializes_without_tool_fields() {
        let v = serde_json::to_value(ChatMessage::text("user", "hi")).unwrap();
        assert_eq!(v["content"], "hi");
        assert!(
            v.get("tool_calls").is_none(),
            "empty tool_calls omitted: {v}"
        );
        assert!(v.get("tool_call_id").is_none());
    }

    #[test]
    fn wants_tools_respects_tool_choice_none_and_forced_tool() {
        let mut req: ChatRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [],
            "tools": [{"type":"function","function":{"name":"t1"}}]
        }))
        .unwrap();
        assert!(req.wants_tools());
        assert_eq!(req.forced_tool(), None);

        req.tool_choice = Some(serde_json::json!("none"));
        assert!(!req.wants_tools());

        req.tool_choice = Some(serde_json::json!({"type":"function","function":{"name":"t1"}}));
        assert!(req.wants_tools());
        assert_eq!(req.forced_tool(), Some("t1"));
    }
}
