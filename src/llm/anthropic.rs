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

use super::types::{
    ChatChoice, ChatMessage, ChatRequest, ChatResponse, EmbeddingsResponse, Tool, ToolCall,
    ToolCallFunction, Usage,
};
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

    /// The Messages-API request body. Identical for buffered and streamed
    /// calls except the `stream` flag.
    fn chat_body(&self, req: &ChatRequest, stream: bool) -> serde_json::Value {
        let model = strip_prefix(&req.model, &self.name);
        let (system, messages) = map_messages(&req.messages);
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": messages,
        });
        if stream {
            body["stream"] = serde_json::json!(true);
        }
        if !system.is_empty() {
            body["system"] = serde_json::json!(system);
        }
        // `tool_choice: "none"` (wants_tools() == false) omits tools entirely.
        if req.wants_tools() {
            body["tools"] = serde_json::json!(map_tools(&req.tools));
            if let Some(tc) = req.tool_choice.as_ref().and_then(map_tool_choice) {
                body["tool_choice"] = tc;
            }
        }
        body
    }

    /// POST the body and normalize connect/HTTP failures into `ProviderError`.
    async fn send_chat(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ProviderError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut rb = self
            .client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body);
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
        Ok(resp)
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let resp = self.send_chat(&self.chat_body(req, false)).await?;
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
        let tool_calls: Vec<ToolCall> = parsed
            .content
            .iter()
            .filter(|b| b.block_type == "tool_use")
            .map(|b| ToolCall {
                id: b.id.clone(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: b.name.clone(),
                    // OpenAI carries arguments as a JSON-encoded string.
                    arguments: if b.input.is_null() {
                        "{}".into()
                    } else {
                        b.input.to_string()
                    },
                },
            })
            .collect();
        let finish_reason = match parsed.stop_reason.as_deref() {
            Some("max_tokens") => "length",
            Some("tool_use") => "tool_calls",
            _ => "stop",
        }
        .to_string();

        let message = ChatMessage {
            role: "assistant".into(),
            // A pure tool-call turn is `content: null` on the OpenAI wire.
            content: if content.is_empty() && !tool_calls.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls,
            tool_call_id: None,
            name: None,
        };
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
                message,
                finish_reason,
            }],
            usage: Usage {
                prompt_tokens: parsed.usage.input_tokens,
                completion_tokens: parsed.usage.output_tokens,
                total_tokens: parsed.usage.input_tokens + parsed.usage.output_tokens,
            },
        })
    }

    /// Native streaming, translated on the fly: Anthropic's SSE events
    /// (`message_start` / `content_block_*` / `message_delta`) become OpenAI
    /// `chat.completion.chunk` frames — text deltas, tool_use → indexed
    /// `tool_calls` deltas with incremental `arguments` fragments, and a final
    /// usage chunk carrying the exact token counts — so any OpenAI streaming
    /// client gets token-level latency from Claude with zero code changes.
    pub async fn chat_stream(
        &self,
        req: &ChatRequest,
    ) -> Result<
        impl futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send + 'static,
        ProviderError,
    > {
        if req.messages.is_empty() {
            return Err(ProviderError::BadRequest(
                "chat request has no messages".into(),
            ));
        }
        let resp = self.send_chat(&self.chat_body(req, true)).await?;
        let name = self.name.clone();
        Ok(TranslatedStream {
            inner: Box::pin(futures_util::StreamExt::map(resp.bytes_stream(), {
                let name = name.clone();
                move |r| r.map_err(|e| ProviderError::Unavailable(name.clone(), e.to_string()))
            })),
            translator: SseTranslator::new(req.model.clone()),
            line_buf: String::new(),
            out: std::collections::VecDeque::new(),
            done: false,
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

// ─────────────── Anthropic SSE → OpenAI chunk translation ───────────────────

/// Cap on the SSE line-reassembly buffer (same rationale as the gateway tee).
const LINE_BUF_CAP: usize = 1 << 20;

/// Stateful translator: feed it the JSON payload of each upstream `data:`
/// line, get back zero or more complete OpenAI SSE frames.
struct SseTranslator {
    model: String,
    id: String,
    created: i64,
    input_tokens: u32,
    output_tokens: u32,
    finish: &'static str,
    /// Anthropic content-block index → OpenAI tool_calls index (text blocks
    /// don't consume a tool index).
    tool_index: std::collections::HashMap<u64, usize>,
    next_tool_index: usize,
    emitted_done: bool,
}

impl SseTranslator {
    fn new(model: String) -> Self {
        SseTranslator {
            model,
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            input_tokens: 0,
            output_tokens: 0,
            finish: "stop",
            tool_index: std::collections::HashMap::new(),
            next_tool_index: 0,
            emitted_done: false,
        }
    }

    fn frame(&self, delta: serde_json::Value, finish: Option<&str>) -> String {
        let chunk = serde_json::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        format!("data: {chunk}\n\n")
    }

    fn feed(&mut self, data: &str) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            return vec![];
        };
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.input_tokens =
                    v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                vec![self.frame(serde_json::json!({ "role": "assistant" }), None)]
            }
            "content_block_start" => {
                let block = &v["content_block"];
                if block["type"] == "tool_use" {
                    let idx = self.next_tool_index;
                    self.next_tool_index += 1;
                    self.tool_index
                        .insert(v["index"].as_u64().unwrap_or(0), idx);
                    vec![self.frame(
                        serde_json::json!({ "tool_calls": [{
                            "index": idx,
                            "id": block["id"],
                            "type": "function",
                            "function": { "name": block["name"], "arguments": "" },
                        }]}),
                        None,
                    )]
                } else {
                    vec![]
                }
            }
            "content_block_delta" => {
                let delta = &v["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        vec![self.frame(serde_json::json!({ "content": delta["text"] }), None)]
                    }
                    "input_json_delta" => {
                        let Some(&idx) = self.tool_index.get(&v["index"].as_u64().unwrap_or(0))
                        else {
                            return vec![];
                        };
                        vec![self.frame(
                            serde_json::json!({ "tool_calls": [{
                                "index": idx,
                                "function": { "arguments": delta["partial_json"] },
                            }]}),
                            None,
                        )]
                    }
                    _ => vec![], // thinking/signature deltas etc. — not chat content
                }
            }
            "message_delta" => {
                if let Some(out) = v["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = out as u32;
                }
                self.finish = match v["delta"]["stop_reason"].as_str() {
                    Some("max_tokens") => "length",
                    Some("tool_use") => "tool_calls",
                    _ => "stop",
                };
                vec![]
            }
            "message_stop" => self.finalize(),
            _ => vec![], // ping, content_block_stop, error frames handled upstream
        }
    }

    /// The terminal frames: finish_reason chunk, exact-usage chunk (the same
    /// shape OpenAI emits with stream_options.include_usage — the gateway's
    /// metering tee reads it), and the [DONE] sentinel.
    fn finalize(&mut self) -> Vec<String> {
        if self.emitted_done {
            return vec![];
        }
        self.emitted_done = true;
        let usage_chunk = serde_json::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
            },
        });
        vec![
            self.frame(serde_json::json!({}), Some(self.finish)),
            format!("data: {usage_chunk}\n\n"),
            "data: [DONE]\n\n".to_string(),
        ]
    }
}

/// Wraps the upstream byte stream: reassembles SSE lines, feeds each `data:`
/// payload to the translator, and yields the translated OpenAI frames.
struct TranslatedStream {
    inner: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>,
    >,
    translator: SseTranslator,
    line_buf: String,
    out: std::collections::VecDeque<bytes::Bytes>,
    done: bool,
}

impl futures_util::Stream for TranslatedStream {
    type Item = Result<bytes::Bytes, ProviderError>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        loop {
            if let Some(frame) = self.out.pop_front() {
                return Poll::Ready(Some(Ok(frame)));
            }
            if self.done {
                return Poll::Ready(None);
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if self.line_buf.len() > LINE_BUF_CAP {
                        // Misbehaving upstream: stop translating, end cleanly.
                        let frames = self.translator.finalize();
                        self.out.extend(frames.into_iter().map(bytes::Bytes::from));
                        self.done = true;
                        continue;
                    }
                    self.line_buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(nl) = self.line_buf.find('\n') {
                        let line: String = self.line_buf.drain(..=nl).collect();
                        if let Some(data) = line.trim().strip_prefix("data: ") {
                            let frames = self.translator.feed(data);
                            self.out.extend(frames.into_iter().map(bytes::Bytes::from));
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    // Upstream ended without message_stop — still close the
                    // OpenAI stream correctly.
                    let frames = self.translator.finalize();
                    self.out.extend(frames.into_iter().map(bytes::Bytes::from));
                    self.done = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
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

/// Map OpenAI-style messages into Anthropic's (system, messages) shape:
///   - system turns concatenate into the top-level `system` string;
///   - assistant turns with `tool_calls` become `tool_use` content blocks;
///   - `role: "tool"` results become `tool_result` blocks in a user turn,
///     merging consecutive results into ONE user turn (Anthropic requires
///     strictly alternating roles);
///   - plain user/assistant turns pass through.
fn map_messages(messages: &[ChatMessage]) -> (String, Vec<serde_json::Value>) {
    let system = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.text_content())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut turns: Vec<serde_json::Value> = Vec::new();
    for m in messages.iter().filter(|m| m.role != "system") {
        if m.role == "tool" {
            let block = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.text_content(),
            });
            if let Some(last) = turns.last_mut() {
                if last["role"] == "user" && last["content"].is_array() {
                    last["content"].as_array_mut().expect("checked").push(block);
                    continue;
                }
            }
            turns.push(serde_json::json!({ "role": "user", "content": [block] }));
        } else if !m.tool_calls.is_empty() {
            let mut blocks = Vec::new();
            if !m.text_content().is_empty() {
                blocks.push(serde_json::json!({ "type": "text", "text": m.text_content() }));
            }
            for c in &m.tool_calls {
                let input: serde_json::Value = serde_json::from_str(&c.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": c.id,
                    "name": c.function.name,
                    "input": input,
                }));
            }
            turns.push(serde_json::json!({ "role": "assistant", "content": blocks }));
        } else {
            turns.push(serde_json::json!({ "role": m.role, "content": m.text_content() }));
        }
    }
    (system, turns)
}

/// OpenAI `tools[]` → Anthropic `tools[]` (`parameters` → `input_schema`).
fn map_tools(tools: &[Tool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            let mut tool = serde_json::json!({
                "name": t.function.name,
                "input_schema": t.function.parameters.clone()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
            });
            if let Some(d) = &t.function.description {
                tool["description"] = serde_json::json!(d);
            }
            tool
        })
        .collect()
}

/// OpenAI `tool_choice` → Anthropic `tool_choice`. `"none"` never reaches here
/// (the caller omits tools entirely); unknown shapes fall back to omitting.
fn map_tool_choice(tc: &serde_json::Value) -> Option<serde_json::Value> {
    match tc.as_str() {
        Some("auto") => Some(serde_json::json!({ "type": "auto" })),
        Some("required") => Some(serde_json::json!({ "type": "any" })),
        Some(_) => None,
        None => tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .map(|name| serde_json::json!({ "type": "tool", "name": name })),
    }
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
    // `tool_use` block fields.
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    input: serde_json::Value,
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
    fn map_messages_separates_system_from_turns() {
        let msgs = vec![
            ChatMessage::text("system", "be terse"),
            ChatMessage::text("user", "hi"),
            ChatMessage::text("assistant", "hello"),
        ];
        let (system, turns) = map_messages(&msgs);
        assert_eq!(system, "be terse");
        assert_eq!(turns.len(), 2, "system turn must be removed from messages");
        assert_eq!(turns[0]["role"], "user");
    }

    #[test]
    fn map_messages_converts_tool_turns_to_anthropic_blocks() {
        let assistant_call: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup_order", "arguments": "{\"order_id\":\"42\"}"}
            }]
        }))
        .unwrap();
        let tool_result: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "tool", "tool_call_id": "call_1", "content": "shipped"
        }))
        .unwrap();
        let second_result: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "tool", "tool_call_id": "call_2", "content": "in stock"
        }))
        .unwrap();

        let msgs = vec![
            ChatMessage::text("user", "where is order 42?"),
            assistant_call,
            tool_result,
            second_result,
        ];
        let (_, turns) = map_messages(&msgs);
        assert_eq!(
            turns.len(),
            3,
            "consecutive tool results merge into one user turn"
        );

        let call_turn = &turns[1];
        assert_eq!(call_turn["role"], "assistant");
        assert_eq!(call_turn["content"][0]["type"], "tool_use");
        assert_eq!(call_turn["content"][0]["name"], "lookup_order");
        assert_eq!(
            call_turn["content"][0]["input"]["order_id"], "42",
            "arguments string must decode into the input object"
        );

        let result_turn = &turns[2];
        assert_eq!(result_turn["role"], "user");
        assert_eq!(result_turn["content"][0]["type"], "tool_result");
        assert_eq!(result_turn["content"][0]["tool_use_id"], "call_1");
        assert_eq!(result_turn["content"][1]["tool_use_id"], "call_2");
    }

    #[test]
    fn maps_openai_tools_and_tool_choice_to_anthropic_shape() {
        let tools: Vec<Tool> = vec![serde_json::from_value(serde_json::json!({
            "type": "function",
            "function": {
                "name": "lookup_order",
                "description": "find an order",
                "parameters": {"type": "object", "properties": {"order_id": {"type": "string"}}}
            }
        }))
        .unwrap()];
        let mapped = map_tools(&tools);
        assert_eq!(mapped[0]["name"], "lookup_order");
        assert_eq!(mapped[0]["description"], "find an order");
        assert_eq!(mapped[0]["input_schema"]["type"], "object");
        assert!(
            mapped[0].get("parameters").is_none(),
            "must rename to input_schema"
        );

        assert_eq!(
            map_tool_choice(&serde_json::json!("auto")).unwrap()["type"],
            "auto"
        );
        assert_eq!(
            map_tool_choice(&serde_json::json!("required")).unwrap()["type"],
            "any"
        );
        let forced =
            map_tool_choice(&serde_json::json!({"type":"function","function":{"name":"x"}}))
                .unwrap();
        assert_eq!(forced["type"], "tool");
        assert_eq!(forced["name"], "x");
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
            messages: vec![ChatMessage::text("user", "hi")],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
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
        assert_eq!(out.choices[0].message.text_content(), "hello from claude");
        assert_eq!(out.choices[0].finish_reason, "stop");
        assert_eq!(out.usage.prompt_tokens, 5);
        assert_eq!(out.usage.completion_tokens, 3);
        assert_eq!(out.usage.total_tokens, 8);
        // model echoes what the client sent (routed form)
        assert_eq!(out.model, "anthropic/claude-opus-4-8");
    }

    #[tokio::test]
    async fn tool_use_response_maps_to_openai_tool_calls() {
        let resp = serde_json::json!({
            "id": "msg_t",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "lookup_order",
                 "input": {"order_id": "42"}}
            ],
            "stop_reason": "tool_use",
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
        let mut r = req("anthropic/claude-opus-4-8");
        r.tools = vec![serde_json::from_value(serde_json::json!({
            "type": "function", "function": {"name": "lookup_order"}
        }))
        .unwrap()];
        let out = p.chat(&r).await.unwrap();

        assert_eq!(out.choices[0].finish_reason, "tool_calls");
        assert_eq!(
            out.choices[0].message.content, None,
            "pure tool turn is content: null"
        );
        let call = &out.choices[0].message.tool_calls[0];
        assert_eq!(call.id, "toolu_1");
        assert_eq!(call.function.name, "lookup_order");
        let args: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(args["order_id"], "42");
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
