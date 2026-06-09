//! LLM gateway — provider routing behind an OpenAI-compatible surface.
//!
//! Riz's "AI gateway" slot: one config block ([gateway]) declares a set of
//! providers and a fallback chain; the OpenAI-compatible HTTP endpoint
//! (`/_riz/v1/*`, see src/system/openai_compat.rs) and every runtime's
//! `ctx.invokeModel` route through here. v1 ships a deterministic `mock`
//! provider (no network — for CI, demos, and offline dev) plus the real
//! Anthropic / OpenAI / Ollama providers.
//!
//! Provider dispatch is an enum (not `dyn`) — the set is small and fixed, so
//! enum dispatch keeps it dependency-free and dyn-compatible without async-trait.

use std::collections::HashMap;
use std::sync::Mutex;

pub mod cost;
pub mod mock;
pub mod openai;
pub mod types;

pub use types::{ChatRequest, ChatResponse, EmbeddingsRequest, EmbeddingsResponse};

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
    /// Constructed by the real HTTP providers (follow-up commits).
    #[allow(dead_code)]
    #[error("provider '{0}' returned an error: {1}")]
    Upstream(String, String),
    /// The request itself is invalid (e.g. no messages) — NOT a fallback candidate.
    #[error("invalid request: {0}")]
    BadRequest(String),
    /// Cumulative spend reached the configured `budget_usd` cap (→ HTTP 412).
    #[error("budget exceeded: cumulative spend reached the configured budget_usd cap")]
    BudgetExceeded,
}

/// A configured provider. One variant per supported backend; the real HTTP
/// providers (OpenAI/Anthropic/Ollama) land in follow-up commits.
#[derive(Debug)]
pub enum Provider {
    Mock(MockProvider),
    /// OpenAI-compatible upstream (serves both `openai` and `ollama` kinds).
    OpenAi(OpenAiProvider),
}

impl Provider {
    // Used for log/introspection once multiple provider kinds ship.
    #[allow(dead_code)]
    pub fn kind(&self) -> &'static str {
        match self {
            Provider::Mock(_) => "mock",
            Provider::OpenAi(_) => "openai-compatible",
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        match self {
            Provider::Mock(p) => p.chat(req).await,
            Provider::OpenAi(p) => p.chat(req).await,
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
    fn record_usage(&self, provider: &str, model: &str, tokens_in: u32, tokens_out: u32) {
        if let Ok(mut ledger) = self.usage.lock() {
            let entry = ledger.entry(provider.to_string()).or_default();
            entry.requests += 1;
            entry.tokens_in += tokens_in as u64;
            entry.tokens_out += tokens_out as u64;
            entry.cost_usd += cost::cost_usd(model, tokens_in, tokens_out);
        }
    }

    /// Build a gateway from the parsed `[gateway]` config. Real HTTP providers
    /// (openai/anthropic/ollama) are added in follow-up commits; until then an
    /// unimplemented kind is a clear build error rather than a silent no-op.
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
        Ok(Gateway::new(providers, default_provider, cfg.fallback_chain.clone())
            .with_budget(cfg.budget_usd))
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
            let provider = &self.providers[&name];
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
        Err(last_err.unwrap())
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
            return Err(ProviderError::BadRequest("embeddings input is empty".into()));
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
            match self.providers[&name].embed(&model, inputs.clone()).await {
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
        Err(last_err.unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::types::ChatMessage;
    use super::*;

    fn user_req(model: &str, content: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: content.into(),
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
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
        assert!(resp.choices[0].message.content.contains("hello"));
        assert!(resp.usage.total_tokens > 0);
    }

    #[tokio::test]
    async fn explicit_provider_prefix_routes_to_that_provider() {
        let gw = mock_gateway();
        // "mock/anything" → mock provider (the only one configured here).
        let resp = gw.chat(&user_req("mock/whatever", "ping")).await.unwrap();
        assert!(resp.choices[0].message.content.contains("ping"));
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
        assert!(resp.choices[0].message.content.contains("hi"));
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
            resp.choices[0].message.content.contains("[mock:"),
            "fallback must reach the mock provider; got: {}",
            resp.choices[0].message.content
        );
    }

    #[tokio::test]
    async fn records_usage_and_enforces_budget() {
        let mut providers = HashMap::new();
        providers.insert("mock".to_string(), Provider::Mock(MockProvider));
        let gw = Gateway::new(providers, "mock".into(), vec!["mock".into()])
            .with_budget(Some(0.000001));
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
}
