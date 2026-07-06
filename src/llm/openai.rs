//! OpenAI-compatible upstream provider. Serves both the `openai` kind (against
//! api.openai.com or any OpenAI-compatible gateway) and the `ollama` kind
//! (against a local Ollama's `/v1` endpoint) — same wire format, different
//! base URL and auth.

use super::types::{ChatRequest, EmbeddingsResponse};
use super::{
    error_snippet, read_body_capped, ChatResponse, ProviderError, MAX_RESPONSE_BODY_BYTES,
};

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

    /// The chat-completions request body. Identical for buffered and streamed
    /// calls except the `stream` flag; streamed requests also ask for the
    /// final usage chunk (`stream_options.include_usage`) so the gateway can
    /// meter them exactly.
    fn chat_body(&self, req: &ChatRequest, stream: bool) -> serde_json::Value {
        let model = strip_prefix(&req.model, &self.name);
        let mut body = serde_json::json!({
            "model": model,
            "messages": req.messages,
            "stream": stream,
        });
        // `json!({…})` always builds an object; `as_object_mut` states that
        // without the panicking `body[…] = …` IndexMut route (rule 9).
        if let Some(obj) = body.as_object_mut() {
            if stream {
                obj.insert(
                    "stream_options".into(),
                    serde_json::json!({ "include_usage": true }),
                );
            }
            if let Some(t) = req.temperature {
                obj.insert("temperature".into(), serde_json::json!(t));
            }
            if let Some(m) = req.max_tokens {
                obj.insert("max_tokens".into(), serde_json::json!(m));
            }
            // Tool calling passes through verbatim — same wire format upstream.
            if !req.tools.is_empty() {
                obj.insert("tools".into(), serde_json::json!(req.tools));
            }
            if let Some(tc) = &req.tool_choice {
                obj.insert("tool_choice".into(), tc.clone());
            }
        }
        body
    }

    /// POST the body and normalize connect/HTTP failures into `ProviderError`.
    async fn send_chat(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut rb = self.client.post(&url).json(body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Unavailable(self.name.clone(), e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let txt = error_snippet(resp, &self.name).await;
            return Err(ProviderError::Upstream(
                self.name.clone(),
                format!("HTTP {status}: {txt}"),
            ));
        }
        Ok(resp)
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let resp = self.send_chat(&self.chat_body(req, false)).await?;
        // Capped read (rule 3): a misbehaving upstream body is a clean
        // Upstream error, not unbounded gateway memory.
        let body = read_body_capped(resp, &self.name, MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice::<ChatResponse>(&body).map_err(|e| {
            ProviderError::Upstream(self.name.clone(), format!("malformed response: {e}"))
        })
    }

    /// Native SSE passthrough: send with `stream: true` and return the upstream
    /// byte stream verbatim — it is already the OpenAI `chat.completion.chunk`
    /// wire format, so nothing needs re-encoding. Errors before any byte flows
    /// (connect failure, non-2xx) surface as normal `ProviderError`s and remain
    /// fallback candidates; once the stream is returned, transport errors
    /// surface as stream items.
    pub async fn chat_stream(
        &self,
        req: &ChatRequest,
    ) -> Result<
        impl futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send + 'static,
        ProviderError,
    > {
        use futures_util::StreamExt;
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let resp = self.send_chat(&self.chat_body(req, true)).await?;
        let name = self.name.clone();
        Ok(resp
            .bytes_stream()
            .map(move |r| r.map_err(|e| ProviderError::Unavailable(name.clone(), e.to_string()))))
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
            let txt = error_snippet(resp, &self.name).await;
            return Err(ProviderError::Upstream(
                self.name.clone(),
                format!("HTTP {status}: {txt}"),
            ));
        }
        // Capped read (rule 3) — same rationale as `chat`.
        let body = read_body_capped(resp, &self.name, MAX_RESPONSE_BODY_BYTES).await?;
        serde_json::from_slice::<EmbeddingsResponse>(&body).map_err(|e| {
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
            messages: vec![ChatMessage::text("user", "hi")],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
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
        assert_eq!(out.choices[0].message.text_content(), "hi from upstream");
        assert_eq!(out.usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn forwards_tools_and_parses_tool_calls_response() {
        // Upstream echoes the request body into a header-free capture and
        // answers with an OpenAI tool-call turn (content: null).
        let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
        let base = spawn(axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                tx.send(body).unwrap();
                async {
                    axum::Json(serde_json::json!({
                        "id": "chatcmpl-t",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4o",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": "call_up1",
                                    "type": "function",
                                    "function": {"name": "lookup_order", "arguments": "{\"order_id\":\"42\"}"}
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                    }))
                }
            }),
        ))
        .await;

        let mut r = req("openai/gpt-4o");
        r.tools = vec![serde_json::from_value(serde_json::json!({
            "type": "function",
            "function": {"name": "lookup_order", "parameters": {"type": "object"}}
        }))
        .unwrap()];
        r.tool_choice = Some(serde_json::json!("auto"));

        let p = OpenAiProvider::new("openai".into(), base, None);
        let out = p.chat(&r).await.expect("tool-call response must parse");

        let sent = rx.recv().unwrap();
        assert_eq!(sent["tools"][0]["function"]["name"], "lookup_order");
        assert_eq!(sent["tool_choice"], "auto");

        assert_eq!(out.choices[0].finish_reason, "tool_calls");
        assert_eq!(out.choices[0].message.content, None);
        assert_eq!(out.choices[0].message.tool_calls[0].id, "call_up1");
        assert_eq!(
            out.choices[0].message.tool_calls[0].function.arguments,
            "{\"order_id\":\"42\"}"
        );
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
