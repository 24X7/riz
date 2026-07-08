//! LLM gateway — provider routing behind an OpenAI-compatible surface.
//!
//! Riz's "AI gateway" slot: one config block ([gateway]) declares a set of
//! providers and a fallback chain; the OpenAI-compatible HTTP endpoint
//! (`/_riz/v1/*`, see src/system/openai_compat.rs) routes through here —
//! handlers call it like any OpenAI server, by base_url. v1 ships a
//! deterministic `mock` provider (no network — for CI, demos, and offline
//! dev) plus the real Anthropic / OpenAI / Ollama providers, with OpenAI
//! function-calling (`tools` / `tool_choice` / `tool_calls`) mapped across
//! all of them.
//!
//! Provider dispatch is an enum (not `dyn`) — the set is small and fixed, so
//! enum dispatch keeps it dependency-free and dyn-compatible without async-trait.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures_util::Stream;

pub mod anthropic;
pub mod cost;
pub mod mock;
pub mod openai;
pub mod types;

pub use types::{ChatRequest, ChatResponse, EmbeddingsRequest, EmbeddingsResponse, Usage};

use anthropic::AnthropicProvider;
use cost::ProviderUsage;
use mock::MockProvider;
use openai::OpenAiProvider;

/// A provider error, tagged with the provider name so the gateway can log which
/// hop failed and decide whether to fall back.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Transport/availability failure — a candidate for fallback.
    #[error("provider '{0}' unavailable: {1}")]
    Unavailable(String, String),
    /// Upstream returned an error response — also a fallback candidate.
    #[error("provider '{0}' returned an error: {1}")]
    Upstream(String, String),
    /// The request itself is invalid (e.g. no messages) — NOT a fallback candidate.
    #[error("invalid request: {0}")]
    BadRequest(String),
    /// Cumulative spend reached the configured `budget_usd` cap (→ HTTP 412).
    #[error("budget exceeded: cumulative spend reached the configured budget_usd cap")]
    BudgetExceeded,
}

/// A configured provider. One variant per supported backend — the mock plus
/// real HTTP upstreams (OpenAI-compatible, Ollama, Anthropic).
#[derive(Debug)]
pub enum Provider {
    Mock(MockProvider),
    /// OpenAI-compatible upstream (serves both `openai` and `ollama` kinds).
    OpenAi(OpenAiProvider),
    /// Anthropic Messages API (maps to/from the OpenAI shape).
    Anthropic(AnthropicProvider),
}

impl Provider {
    // Introspection helper (provider kind as a static str) kept for logging;
    // no call site yet, hence the allow.
    #[allow(dead_code)]
    pub fn kind(&self) -> &'static str {
        match self {
            Provider::Mock(_) => "mock",
            Provider::OpenAi(_) => "openai-compatible",
            Provider::Anthropic(_) => "anthropic",
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        match self {
            Provider::Mock(p) => p.chat(req).await,
            Provider::OpenAi(p) => p.chat(req).await,
            Provider::Anthropic(p) => p.chat(req).await,
        }
    }

    pub async fn embed(
        &self,
        model: &str,
        inputs: Vec<String>,
    ) -> Result<EmbeddingsResponse, ProviderError> {
        match self {
            Provider::Mock(p) => p.embed(model, inputs).await,
            Provider::OpenAi(p) => p.embed(model, inputs).await,
            Provider::Anthropic(p) => p.embed(model, inputs).await,
        }
    }
}

/// The gateway: a named set of providers, a default, and a fallback chain.
#[derive(Debug)]
pub struct Gateway {
    providers: HashMap<String, Provider>,
    default_provider: String,
    fallback_chain: Vec<String>,
    /// Optional cumulative spend cap (USD). When set and reached, further
    /// requests are rejected with [`ProviderError::BudgetExceeded`].
    budget_usd: Option<f64>,
    /// Cumulative per-provider usage ledger (requests, tokens, cost).
    usage: Mutex<HashMap<String, ProviderUsage>>,
}

impl Gateway {
    pub fn new(
        providers: HashMap<String, Provider>,
        default_provider: String,
        fallback_chain: Vec<String>,
    ) -> Self {
        Self {
            providers,
            default_provider,
            fallback_chain,
            budget_usd: None,
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// Set the cumulative spend cap.
    pub fn with_budget(mut self, budget_usd: Option<f64>) -> Self {
        self.budget_usd = budget_usd;
        self
    }

    /// Total spend so far across all providers (USD).
    pub fn total_cost_usd(&self) -> f64 {
        self.usage
            .lock()
            .map(|u| u.values().map(|p| p.cost_usd).sum())
            .unwrap_or(0.0)
    }

    /// Snapshot for `GET /_riz/v1/usage`: (budget, total cost, per-provider).
    pub fn usage_snapshot(&self) -> (Option<f64>, f64, HashMap<String, ProviderUsage>) {
        let map = self.usage.lock().map(|u| u.clone()).unwrap_or_default();
        let total = map.values().map(|p| p.cost_usd).sum();
        (self.budget_usd, total, map)
    }

    /// True when a budget is set and cumulative spend has reached it.
    fn over_budget(&self) -> bool {
        matches!(self.budget_usd, Some(cap) if self.total_cost_usd() >= cap)
    }

    /// Record a successful call's tokens + cost against a provider.
    ///
    /// Saturating arithmetic throughout: the ledger backs the budget cap, so
    /// a saturated counter stays HIGH and keeps over-counting spend (fails
    /// closed) — wrapping would reset it and silently re-open the budget.
    fn record_usage(&self, provider: &str, model: &str, tokens_in: u32, tokens_out: u32) {
        if let Ok(mut ledger) = self.usage.lock() {
            let entry = ledger.entry(provider.to_string()).or_default();
            entry.requests = entry.requests.saturating_add(1);
            entry.tokens_in = entry.tokens_in.saturating_add(u64::from(tokens_in));
            entry.tokens_out = entry.tokens_out.saturating_add(u64::from(tokens_out));
            entry.cost_usd += cost::cost_usd(model, tokens_in, tokens_out);
        }
    }

    /// Build a gateway from the parsed `[gateway]` config. Each provider kind
    /// (mock/openai/ollama/anthropic) maps to its backend here; an unknown kind
    /// is a clear build error rather than a silent no-op.
    pub fn from_config(cfg: &crate::config::GatewayConfig) -> Result<Self, String> {
        let mut providers = HashMap::new();
        for (name, pc) in &cfg.providers {
            let provider = match pc.kind.as_str() {
                "mock" => Provider::Mock(MockProvider),
                "openai" => {
                    let base_url = pc
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.openai.com/v1".into());
                    let api_key = pc.api_key_env.as_ref().and_then(|v| std::env::var(v).ok());
                    Provider::OpenAi(OpenAiProvider::new(name.clone(), base_url, api_key))
                }
                "ollama" => {
                    let base_url = pc
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "http://localhost:11434/v1".into());
                    Provider::OpenAi(OpenAiProvider::new(name.clone(), base_url, None))
                }
                "anthropic" => {
                    let base_url = pc
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.anthropic.com".into());
                    let api_key = pc.api_key_env.as_ref().and_then(|v| std::env::var(v).ok());
                    Provider::Anthropic(AnthropicProvider::new(name.clone(), base_url, api_key))
                }
                other => {
                    return Err(format!(
                        "[gateway.providers.{name}] kind '{other}' is not yet implemented"
                    ))
                }
            };
            providers.insert(name.clone(), provider);
        }
        let default_provider = cfg
            .default_provider
            .clone()
            .or_else(|| {
                let mut names: Vec<&String> = providers.keys().collect();
                names.sort();
                names.first().map(|s| (*s).clone())
            })
            .unwrap_or_default();
        Ok(
            Gateway::new(providers, default_provider, cfg.fallback_chain.clone())
                .with_budget(cfg.budget_usd),
        )
    }

    /// Names of all configured providers (for `GET /_riz/v1/models`).
    pub fn provider_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.providers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Resolve the primary provider name for a request's `model` field. A model
    /// of the form `"<provider>/<model>"` routes explicitly to that provider;
    /// otherwise the default provider handles it.
    fn route(&self, model: &str) -> String {
        match model.split_once('/') {
            Some((provider, _)) if self.providers.contains_key(provider) => provider.to_string(),
            _ => self.default_provider.clone(),
        }
    }

    /// Public view of which provider a model routes to — used by the
    /// chat-completion handler to tag the local token read-model (and the
    /// `--dev` TUI's recent-calls list) with the serving provider.
    pub fn resolved_provider(&self, model: &str) -> String {
        self.route(model)
    }

    /// The ordered list of providers to attempt: the routed one first, then the
    /// fallback chain, de-duplicated, skipping any that aren't configured.
    fn attempt_order(&self, model: &str) -> Vec<String> {
        let mut order = Vec::new();
        let primary = self.route(model);
        if self.providers.contains_key(&primary) {
            order.push(primary);
        }
        for name in &self.fallback_chain {
            if self.providers.contains_key(name) && !order.contains(name) {
                order.push(name.clone());
            }
        }
        order
    }

    /// Route a chat request through the provider chain. Tries each provider in
    /// order; returns the first success. A `BadRequest` short-circuits (no point
    /// falling back). If every provider fails, returns the last error.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        if self.over_budget() {
            return Err(ProviderError::BudgetExceeded);
        }
        let order = self.attempt_order(&req.model);
        if order.is_empty() {
            return Err(ProviderError::Unavailable(
                req.model.clone(),
                "no provider configured for this model and no fallback available".into(),
            ));
        }
        let mut last_err = None;
        for name in order {
            // attempt_order only lists configured names; the table is
            // immutable after startup, so a miss is a logic bug — skip the
            // hop rather than panic the request task.
            let Some(provider) = self.providers.get(&name) else {
                continue;
            };
            match provider.chat(req).await {
                Ok(resp) => {
                    self.record_usage(
                        &name,
                        &req.model,
                        resp.usage.prompt_tokens,
                        resp.usage.completion_tokens,
                    );
                    return Ok(resp);
                }
                Err(e @ ProviderError::BadRequest(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("gateway: provider '{name}' failed: {e}; trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| no_provider_answered(&req.model)))
    }

    /// Route a `stream: true` chat request. Providers with native SSE are
    /// proxied token-by-token — `openai`/`ollama` kinds byte-for-byte, and
    /// `anthropic` translated on the fly to OpenAI chunks — so
    /// time-to-first-token is the provider's, not "after the whole
    /// completion". The mock provider returns a buffered response the HTTP
    /// layer re-emits as synthesized chunks (same SSE contract).
    ///
    /// Fallback semantics: failures BEFORE any byte flows walk the chain
    /// exactly like [`chat`](Self::chat) (an upstream that rejects the
    /// streaming request is retried buffered on the same provider first);
    /// once bytes flow there is no falling back.
    ///
    /// `on_complete` fires exactly once when the outcome's usage is known —
    /// immediately for buffered responses, at stream end (or client
    /// disconnect, best-effort approximated) for proxied streams. The ledger
    /// (`/_riz/v1/usage`, budget) is recorded internally either way.
    pub async fn chat_stream(
        self: &Arc<Self>,
        req: &ChatRequest,
        on_complete: impl FnOnce(Usage) + Send + 'static,
    ) -> Result<ChatStream, ProviderError> {
        if self.over_budget() {
            return Err(ProviderError::BudgetExceeded);
        }
        let order = self.attempt_order(&req.model);
        if order.is_empty() {
            return Err(ProviderError::Unavailable(
                req.model.clone(),
                "no provider configured for this model and no fallback available".into(),
            ));
        }
        let mut last_err = None;
        for name in order {
            match self.stream_attempt(&name, req).await {
                // Moving `on_complete` here is fine: both success arms return,
                // so the loop can never reach a second move.
                Ok(StreamAttempt::Native(stream)) => {
                    let tee = self.metered_tee(name, req, stream, on_complete);
                    return Ok(ChatStream::Upstream(Box::pin(tee)));
                }
                Ok(StreamAttempt::Buffered(resp)) => {
                    self.record_usage(
                        &name,
                        &req.model,
                        resp.usage.prompt_tokens,
                        resp.usage.completion_tokens,
                    );
                    on_complete(resp.usage.clone());
                    return Ok(ChatStream::Buffered(resp));
                }
                Err(e @ ProviderError::BadRequest(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("gateway: provider '{name}' failed: {e}; trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| no_provider_answered(&req.model)))
    }

    /// One provider's shot at a streaming request: a native SSE stream when
    /// the provider supports it, otherwise (or when the upstream refuses the
    /// STREAMING request but may still serve buffered — e.g. an
    /// OpenAI-compatible server without stream_options) a buffered response.
    async fn stream_attempt(
        &self,
        name: &str,
        req: &ChatRequest,
    ) -> Result<StreamAttempt, ProviderError> {
        // attempt_order only lists configured names; the table is immutable
        // after startup, so a miss is a logic bug — fail the hop, not the task.
        let Some(provider) = self.providers.get(name) else {
            return Err(ProviderError::Unavailable(
                name.to_string(),
                "provider not configured".into(),
            ));
        };
        // None = no native streaming → plain buffered call below (the HTTP
        // layer re-emits it as synthesized chunks).
        let native: Option<Result<BoxedByteStream, ProviderError>> = match provider {
            Provider::OpenAi(p) => Some(p.chat_stream(req).await.map(boxed_stream)),
            Provider::Anthropic(p) => Some(p.chat_stream(req).await.map(boxed_stream)),
            Provider::Mock(_) => None,
        };
        match native {
            Some(Ok(stream)) => Ok(StreamAttempt::Native(stream)),
            Some(Err(e @ ProviderError::Upstream(_, _))) => {
                tracing::warn!(
                    "gateway: provider '{name}' rejected the stream request ({e}); retrying buffered"
                );
                provider.chat(req).await.map(StreamAttempt::Buffered)
            }
            Some(Err(e)) => Err(e),
            None => provider.chat(req).await.map(StreamAttempt::Buffered),
        }
    }

    /// Wrap a native upstream stream in the usage-metering tee: forwards
    /// bytes untouched, then records the ledger entry and fires `done`
    /// exactly once when the stream ends (or the client disconnects).
    fn metered_tee(
        self: &Arc<Self>,
        provider: String,
        req: &ChatRequest,
        stream: BoxedByteStream,
        done: impl FnOnce(Usage) + Send + 'static,
    ) -> UsageTee {
        let gw = Arc::clone(self);
        let model = req.model.clone();
        // Saturating fold: the prompt approximation feeds budget metering, so
        // it must never wrap down (fail closed, same as record_usage).
        let prompt_approx = req
            .messages
            .iter()
            .map(|m| types::approx_tokens(m.text_content()))
            .fold(0u32, u32::saturating_add);
        UsageTee {
            inner: stream,
            line_buf: String::new(),
            usage: None,
            approx_completion: 0,
            prompt_approx,
            on_end: Some(Box::new(move |usage: Usage| {
                gw.record_usage(
                    &provider,
                    &model,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                );
                done(usage);
            })),
        }
    }

    /// Route an embeddings request through the provider chain (same routing +
    /// fallback semantics as [`chat`](Self::chat)).
    pub async fn embed(&self, req: EmbeddingsRequest) -> Result<EmbeddingsResponse, ProviderError> {
        if self.over_budget() {
            return Err(ProviderError::BudgetExceeded);
        }
        let model = req.model.clone();
        let inputs = req.input.into_vec();
        if inputs.is_empty() {
            return Err(ProviderError::BadRequest(
                "embeddings input is empty".into(),
            ));
        }
        let order = self.attempt_order(&model);
        if order.is_empty() {
            return Err(ProviderError::Unavailable(
                model,
                "no provider configured and no fallback available".into(),
            ));
        }
        let mut last_err = None;
        for name in order {
            // Same non-panicking lookup rationale as in `chat`.
            let Some(provider) = self.providers.get(&name) else {
                continue;
            };
            match provider.embed(&model, inputs.clone()).await {
                Ok(resp) => {
                    self.record_usage(&name, &model, resp.usage.prompt_tokens, 0);
                    return Ok(resp);
                }
                Err(e @ ProviderError::BadRequest(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("gateway: provider '{name}' embed failed: {e}; trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| no_provider_answered(&model)))
    }
}

/// The terminal error when the fallback chain is exhausted without any
/// provider producing an error to report — structurally impossible today
/// (every attempted hop either returns or records `last_err`), but the
/// gateway degrades with a clean upstream error rather than unwrapping.
fn no_provider_answered(model: &str) -> ProviderError {
    ProviderError::Unavailable(
        model.to_string(),
        "no provider in the chain produced a response".into(),
    )
}

/// Outcome of one provider hop in [`Gateway::chat_stream`].
enum StreamAttempt {
    /// Native upstream SSE — already OpenAI chunk format.
    Native(BoxedByteStream),
    /// Buffered response (no native streaming, or stream refused but the
    /// buffered retry succeeded).
    Buffered(ChatResponse),
}

/// The boxed byte-stream shape every native-streaming provider reduces to.
type BoxedByteStream = Pin<Box<dyn Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>>;

fn boxed_stream<S>(s: S) -> BoxedByteStream
where
    S: Stream<Item = Result<bytes::Bytes, ProviderError>> + Send + 'static,
{
    Box::pin(s)
}

/// Cap on a buffered provider response body (rule 3: no unbounded growth
/// from remote input). Generous — large embedding batches are legitimate —
/// but finite: a misbehaving upstream cannot balloon gateway memory.
pub(crate) const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Cap when reading a non-2xx body: only a ~300-char snippet is ever quoted
/// back, so anything past this is waste.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Read a provider response body into memory, enforcing `cap`. An over-cap
/// body — declared via Content-Length or discovered while streaming — is a
/// clean `Upstream` error, never unbounded accumulation.
pub(crate) async fn read_body_capped(
    resp: reqwest::Response,
    provider: &str,
    cap: usize,
) -> Result<bytes::Bytes, ProviderError> {
    use futures_util::StreamExt;
    if let Some(len) = resp.content_length() {
        if len > cap as u64 {
            return Err(ProviderError::Upstream(
                provider.to_string(),
                format!("response body of {len} bytes exceeds the gateway cap ({cap})"),
            ));
        }
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ProviderError::Unavailable(provider.to_string(), e.to_string()))?;
        // Saturating: the guard must hold even at the usize edge.
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(ProviderError::Upstream(
                provider.to_string(),
                format!("response body exceeds the gateway cap ({cap} bytes)"),
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.into())
}

/// The truncated snippet of a non-2xx provider body quoted into the error
/// envelope. Reads through the small cap; unreadable bodies quote as empty.
pub(crate) async fn error_snippet(resp: reqwest::Response, provider: &str) -> String {
    match read_body_capped(resp, provider, MAX_ERROR_BODY_BYTES).await {
        Ok(b) => String::from_utf8_lossy(&b).chars().take(300).collect(),
        Err(_) => String::new(),
    }
}

/// A streaming chat outcome from [`Gateway::chat_stream`].
pub enum ChatStream {
    /// Native upstream SSE passthrough — already the OpenAI
    /// `chat.completion.chunk` wire format; pipe the bytes to the client
    /// verbatim. Usage is metered by the internal tee when the stream ends.
    Upstream(Pin<Box<dyn Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>>),
    /// No native stream for this provider (mock) — the caller synthesizes
    /// chunks from the buffered response.
    Buffered(ChatResponse),
}

/// Cap on the SSE line-reassembly buffer — a well-formed upstream line is a
/// few KB; anything past this is a misbehaving upstream and we stop scanning
/// (passthrough continues untouched, usage falls back to the approximation).
const TEE_LINE_BUF_CAP: usize = 1 << 20;

/// Read a token-count field from a provider `usage` object. Absent or
/// malformed fields read 0; values beyond `u32::MAX` clamp HIGH (the ledger
/// over-counts — fails closed) instead of the old `as` truncation, which
/// would wrap a hostile count down to a small number.
fn token_field(u: &serde_json::Value, key: &str) -> u32 {
    u.get(key)
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Forwards upstream bytes untouched while scanning SSE lines for the final
/// `usage` chunk (and accumulating an approximate completion-token count as a
/// fallback). Fires `on_end` exactly once — at stream end, or on drop if the
/// client disconnected mid-stream (best effort, approximated).
struct UsageTee {
    inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>>,
    line_buf: String,
    usage: Option<Usage>,
    approx_completion: u32,
    prompt_approx: u32,
    on_end: Option<Box<dyn FnOnce(Usage) + Send>>,
}

impl UsageTee {
    fn scan(&mut self, chunk: &[u8]) {
        if self.line_buf.len() > TEE_LINE_BUF_CAP {
            return; // misbehaving upstream; stop scanning, keep forwarding
        }
        self.line_buf.push_str(&String::from_utf8_lossy(chunk));
        while let Some(nl) = self.line_buf.find('\n') {
            let line: String = self.line_buf.drain(..=nl).collect();
            let Some(data) = line.trim().strip_prefix("data: ") else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                self.usage = Some(Usage {
                    prompt_tokens: token_field(u, "prompt_tokens"),
                    completion_tokens: token_field(u, "completion_tokens"),
                    total_tokens: token_field(u, "total_tokens"),
                });
            } else if let Some(content) = v
                .pointer("/choices/0/delta/content")
                .and_then(serde_json::Value::as_str)
            {
                // Saturating: metering fallback must never wrap down.
                self.approx_completion = self
                    .approx_completion
                    .saturating_add(types::approx_tokens(content));
            }
        }
    }

    fn finish(&mut self) {
        if let Some(done) = self.on_end.take() {
            let usage = self.usage.take().unwrap_or(Usage {
                prompt_tokens: self.prompt_approx,
                completion_tokens: self.approx_completion,
                // Saturating: fail closed (over-count) rather than wrap.
                total_tokens: self.prompt_approx.saturating_add(self.approx_completion),
            });
            done(usage);
        }
    }
}

impl Stream for UsageTee {
    type Item = Result<bytes::Bytes, ProviderError>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.scan(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                self.finish();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for UsageTee {
    fn drop(&mut self) {
        // Client hung up mid-stream: still meter what flowed (best effort).
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::types::ChatMessage;
    use super::*;

    fn user_req(model: &str, content: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", content)],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
        }
    }

    fn mock_gateway() -> Gateway {
        let mut providers = HashMap::new();
        providers.insert("mock".to_string(), Provider::Mock(MockProvider));
        Gateway::new(providers, "mock".into(), vec!["mock".into()])
    }

    #[tokio::test]
    async fn routes_to_default_provider_and_returns_openai_shape() {
        let gw = mock_gateway();
        let resp = gw.chat(&user_req("gpt-4o", "hello")).await.unwrap();
        assert_eq!(resp.object, "chat.completion");
        assert_eq!(resp.model, "gpt-4o");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.role, "assistant");
        assert!(resp.choices[0].message.text_content().contains("hello"));
        assert!(resp.usage.total_tokens > 0);
    }

    #[tokio::test]
    async fn explicit_provider_prefix_routes_to_that_provider() {
        let gw = mock_gateway();
        // "mock/anything" → mock provider (the only one configured here).
        let resp = gw.chat(&user_req("mock/whatever", "ping")).await.unwrap();
        assert!(resp.choices[0].message.text_content().contains("ping"));
    }

    #[tokio::test]
    async fn empty_messages_is_bad_request_not_fallback() {
        let gw = mock_gateway();
        let req = ChatRequest {
            model: "mock".into(),
            messages: vec![],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
        };
        let err = gw.chat(&req).await.unwrap_err();
        assert!(matches!(err, ProviderError::BadRequest(_)));
    }

    #[tokio::test]
    async fn from_config_builds_mock_and_routes() {
        let toml_str = r#"
default_provider = "mock"
fallback_chain = ["mock"]
[providers.mock]
kind = "mock"
"#;
        let cfg: crate::config::GatewayConfig = toml::from_str(toml_str).unwrap();
        let gw = Gateway::from_config(&cfg).expect("builds");
        let resp = gw.chat(&user_req("anything", "hi")).await.unwrap();
        assert!(resp.choices[0].message.text_content().contains("hi"));
    }

    #[test]
    fn from_config_builds_openai_compatible_providers() {
        let toml_str = r#"
[providers.openai]
kind = "openai"
[providers.ollama]
kind = "ollama"
"#;
        let cfg: crate::config::GatewayConfig = toml::from_str(toml_str).unwrap();
        let gw = Gateway::from_config(&cfg).expect("builds openai + ollama");
        let mut names = gw.provider_names();
        names.sort();
        assert_eq!(names, vec!["ollama".to_string(), "openai".to_string()]);
    }

    #[tokio::test]
    async fn falls_back_to_mock_when_primary_provider_errors() {
        // Primary 'openai' points at an unroutable port → Unavailable; the
        // gateway must fall through the chain to 'mock' and succeed.
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            Provider::OpenAi(OpenAiProvider::new(
                "openai".into(),
                "http://127.0.0.1:1/v1".into(),
                None,
            )),
        );
        providers.insert("mock".to_string(), Provider::Mock(MockProvider));
        let gw = Gateway::new(providers, "openai".into(), vec!["mock".into()]);

        let resp = gw.chat(&user_req("openai/gpt-4o", "hi")).await.unwrap();
        assert!(
            resp.choices[0].message.text_content().contains("[mock:"),
            "fallback must reach the mock provider; got: {}",
            resp.choices[0].message.text_content()
        );
    }

    #[tokio::test]
    async fn records_usage_and_enforces_budget() {
        let mut providers = HashMap::new();
        providers.insert("mock".to_string(), Provider::Mock(MockProvider));
        let gw =
            Gateway::new(providers, "mock".into(), vec!["mock".into()]).with_budget(Some(0.000001));
        // First call: budget checked before the call (cost starts at 0) → proceeds.
        gw.chat(&user_req("mock", "hello world")).await.unwrap();
        assert!(gw.total_cost_usd() > 0.0, "usage must record non-zero cost");
        // Cumulative cost now exceeds the tiny cap → next call is rejected.
        let err = gw.chat(&user_req("mock", "again")).await.unwrap_err();
        assert!(matches!(err, ProviderError::BudgetExceeded), "got {err:?}");
    }

    #[tokio::test]
    async fn no_providers_returns_unavailable() {
        let gw = Gateway::new(HashMap::new(), "mock".into(), vec![]);
        let err = gw.chat(&user_req("gpt-4o", "x")).await.unwrap_err();
        assert!(matches!(err, ProviderError::Unavailable(_, _)));
    }

    /// Drive a UsageTee over the given upstream frames; returns the bytes it
    /// forwarded and the Usage it reported to on_end.
    async fn run_tee(frames: Vec<&'static str>, prompt: &str) -> (Vec<bytes::Bytes>, Usage) {
        use futures_util::StreamExt;
        let (tx, rx) = std::sync::mpsc::channel();
        let gw = Arc::new(mock_gateway());
        let req = user_req("mock", prompt);
        let items: Vec<Result<bytes::Bytes, ProviderError>> = frames
            .into_iter()
            .map(|f| Ok(bytes::Bytes::from(f)))
            .collect();
        let tee = gw.metered_tee(
            "mock".into(),
            &req,
            boxed_stream(futures_util::stream::iter(items)),
            move |u| {
                tx.send(u).unwrap();
            },
        );
        let forwarded: Vec<_> = Box::pin(tee)
            .map(|r| r.expect("test frames are all Ok"))
            .collect()
            .await;
        (forwarded, rx.recv().expect("on_end must fire exactly once"))
    }

    #[tokio::test]
    async fn usage_tee_survives_malformed_provider_chunks_and_meters_approx() {
        // A misbehaving upstream: junk JSON, wrong shapes, no usage chunk,
        // no [DONE] — the tee must forward bytes untouched, never panic, and
        // fall back to the approximation when the stream ends.
        let frames = vec![
            "data: {not json}\n",
            "data: {\"choices\": \"not-an-array\"}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"two words\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":42}}]}\n",
            "data: {\"usage\": null}\n",
        ];
        let (forwarded, usage) = run_tee(frames.clone(), "three word prompt").await;
        assert_eq!(forwarded.len(), frames.len(), "passthrough is untouched");
        assert_eq!(usage.prompt_tokens, 3, "approx from the request messages");
        assert_eq!(usage.completion_tokens, 2, "approx from the one text delta");
        assert_eq!(usage.total_tokens, 5);
    }

    #[tokio::test]
    async fn usage_tee_clamps_hostile_usage_counts_high_not_wrapped() {
        // A usage chunk with counts beyond u32 must clamp HIGH (over-count →
        // budget fails closed), not truncate to a small number.
        let frames = vec![
            "data: {\"usage\":{\"prompt_tokens\":18446744073709551615,\
             \"completion_tokens\":7,\"total_tokens\":\"junk\"}}\n",
        ];
        let (_, usage) = run_tee(frames, "hi").await;
        assert_eq!(usage.prompt_tokens, u32::MAX);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 0, "malformed field reads 0, no panic");
    }

    #[tokio::test]
    async fn read_body_capped_rejects_oversized_bodies_cleanly() {
        // /big has a Content-Length (early reject); /chunked streams without
        // one (the accumulation guard rejects mid-read).
        let app = axum::Router::new()
            .route("/big", axum::routing::get(|| async { vec![b'x'; 100_000] }))
            .route(
                "/chunked",
                axum::routing::get(|| async {
                    let chunks = (0..100)
                        .map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from(vec![b'y'; 1000])));
                    axum::body::Body::from_stream(futures_util::stream::iter(chunks))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for path in ["big", "chunked"] {
            let resp = reqwest::get(format!("http://{addr}/{path}")).await.unwrap();
            let err = read_body_capped(resp, "p", 1024).await.unwrap_err();
            assert!(
                matches!(err, ProviderError::Upstream(_, _)),
                "{path}: over-cap must be a clean Upstream error, got {err:?}"
            );
        }
        // Under the cap, the body reads whole.
        let resp = reqwest::get(format!("http://{addr}/big")).await.unwrap();
        let body = read_body_capped(resp, "p", 1 << 20).await.unwrap();
        assert_eq!(body.len(), 100_000);
    }
}
