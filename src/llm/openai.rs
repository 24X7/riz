//! OpenAI-compatible upstream provider. Serves both the `openai` kind (against
//! api.openai.com or any OpenAI-compatible gateway) and the `ollama` kind
//! (against a local Ollama's `/v1` endpoint) — same wire format, different
//! base URL and auth.

use super::types::{ChatRequest, EmbeddingsResponse};
use super::{ChatResponse, ProviderError};

pub struct OpenAiProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl OpenAiProvider {
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
        let mut body = serde_json::json!({
            "model": model,
            "messages": req.messages,
            "stream": false,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(m);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut rb = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Unavailable(self.name.clone(), e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let txt: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
            return Err(ProviderError::Upstream(
                self.name.clone(),
                format!("HTTP {status}: {txt}"),
            ));
        }
        resp.json::<ChatResponse>().await.map_err(|e| {
            ProviderError::Upstream(self.name.clone(), format!("malformed response: {e}"))
        })
    }

    pub async fn embed(
        &self,
        model: &str,
        inputs: Vec<String>,
    ) -> Result<EmbeddingsResponse, ProviderError> {
        let model = strip_prefix(model, &self.name);
        let body = serde_json::json!({ "model": model, "input": inputs });
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let mut rb = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Unavailable(self.name.clone(), e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let txt: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
            return Err(ProviderError::Upstream(
                self.name.clone(),
                format!("HTTP {status}: {txt}"),
            ));
        }
        resp.json::<EmbeddingsResponse>().await.map_err(|e| {
            ProviderError::Upstream(self.name.clone(), format!("malformed response: {e}"))
        })
    }
}

/// Strip a leading `"<provider>/"` so a routed model like `"openai/gpt-4o"`
/// is forwarded upstream as `"gpt-4o"`.
fn strip_prefix(model: &str, name: &str) -> String {
    model
        .strip_prefix(&format!("{name}/"))
        .unwrap_or(model)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::super::types::ChatMessage;
    use super::*;

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

    async fn spawn(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn parses_upstream_openai_response_ignoring_extra_fields() {
        let resp = serde_json::json!({
            "id": "chatcmpl-upstream",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-4o",
            "choices": [{"index":0,"message":{"role":"assistant","content":"hi from upstream"},"finish_reason":"stop","logprobs":null}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7},
            "system_fingerprint": "fp_xxx"
        });
        let base = spawn(axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let r = resp.clone();
                async move { axum::Json(r) }
            }),
        ))
        .await;

        let p = OpenAiProvider::new("openai".into(), base, Some("sk-test".into()));
        let out = p.chat(&req("openai/gpt-4o")).await.unwrap();
        assert_eq!(out.id, "chatcmpl-upstream");
        assert_eq!(out.choices[0].message.content, "hi from upstream");
        assert_eq!(out.usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn http_error_maps_to_upstream_error() {
        let base = spawn(axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")
            }),
        ))
        .await;
        let p = OpenAiProvider::new("openai".into(), base, None);
        let err = p.chat(&req("openai/x")).await.unwrap_err();
        assert!(matches!(err, ProviderError::Upstream(_, _)), "got {err:?}");
    }
}
