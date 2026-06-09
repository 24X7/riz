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

pub mod mock;
pub mod types;

pub use types::{ChatChoice, ChatMessage, ChatRequest, ChatResponse, Usage};

use mock::MockProvider;

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
}

/// A configured provider. One variant per supported backend; the real HTTP
/// providers (OpenAI/Anthropic/Ollama) land in follow-up commits.
#[derive(Debug)]
pub enum Provider {
    Mock(MockProvider),
}

impl Provider {
    pub fn kind(&self) -> &'static str {
        match self {
            Provider::Mock(_) => "mock",
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        match self {
            Provider::Mock(p) => p.chat(req).await,
        }
    }
}

/// The gateway: a named set of providers, a default, and a fallback chain.
#[derive(Debug)]
pub struct Gateway {
    providers: HashMap<String, Provider>,
    default_provider: String,
    fallback_chain: Vec<String>,
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
        Ok(Gateway::new(
            providers,
            default_provider,
            cfg.fallback_chain.clone(),
        ))
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
                Ok(resp) => return Ok(resp),
                Err(e @ ProviderError::BadRequest(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("gateway: provider '{name}' failed: {e}; trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }
}

#[cfg(test)]
mod tests {
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
    fn from_config_errors_on_unimplemented_kind() {
        let toml_str = r#"
[providers.openai]
kind = "openai"
"#;
        let cfg: crate::config::GatewayConfig = toml::from_str(toml_str).unwrap();
        let err = Gateway::from_config(&cfg).unwrap_err();
        assert!(err.contains("not yet implemented"), "got: {err}");
    }

    #[tokio::test]
    async fn no_providers_returns_unavailable() {
        let gw = Gateway::new(HashMap::new(), "mock".into(), vec![]);
        let err = gw.chat(&user_req("gpt-4o", "x")).await.unwrap_err();
        assert!(matches!(err, ProviderError::Unavailable(_, _)));
    }
}
