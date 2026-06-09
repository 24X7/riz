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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
    pub fn assistant(model: &str, content: String, prompt_tokens: u32, completion_tokens: u32) -> Self {
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
                message: ChatMessage {
                    role: "assistant".into(),
                    content,
                },
                finish_reason: "stop".into(),
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        }
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
