//! Per-call cost estimation from a static pricing table ($/token), and the
//! cumulative usage ledger the gateway exposes for AI-FinOps.
//!
//! Prices are illustrative USD per 1M tokens (input, output); refresh as
//! providers change them. The `mock` provider carries a nominal price so
//! cost/budget are demonstrable with no real API.
//!
//! Budget caps FAIL CLOSED: a model the table doesn't know is priced at the
//! table's most expensive tier, not zero — otherwise a new model name (or a
//! typo'd route) silently bypasses `[gateway] budget_usd` entirely. Models
//! routed through the local `ollama/` provider are the explicit exception:
//! they are known-free, not unknown.

use serde::Serialize;

/// Conservative fallback for models the table doesn't know: the most
/// expensive tier listed below. Overcounting an unknown model throttles a
/// budget early; undercounting (the old zero default) disables the cap.
const UNKNOWN_MODEL_PRICE: (f64, f64) = (15.00, 75.00);

/// USD per 1M tokens, as (input, output).
fn price_per_million(model: &str) -> (f64, f64) {
    // Local Ollama models are genuinely free — no metered upstream.
    if model.starts_with("ollama/") {
        return (0.0, 0.0);
    }
    // Route forms like "openai/gpt-4o" → "gpt-4o".
    let m = model.rsplit('/').next().unwrap_or(model);
    match m {
        "mock" => (0.50, 1.50), // nominal, so the demo shows non-zero cost
        s if s.starts_with("gpt-4o-mini") => (0.15, 0.60),
        s if s.starts_with("gpt-4o") => (2.50, 10.00),
        s if s.starts_with("gpt-4") => (30.00, 60.00),
        s if s.starts_with("claude-3-5") || s.starts_with("claude-sonnet") => (3.00, 15.00),
        s if s.starts_with("claude-3-opus") || s.starts_with("claude-opus") => (15.00, 75.00),
        s if s.starts_with("claude-3-haiku") || s.starts_with("claude-haiku") => (0.80, 4.00),
        s if s.starts_with("text-embedding-3-small") => (0.02, 0.0),
        s if s.starts_with("text-embedding-3-large") => (0.13, 0.0),
        _ => UNKNOWN_MODEL_PRICE,
    }
}

/// Estimated USD cost of a call given its model and token counts.
pub fn cost_usd(model: &str, tokens_in: u32, tokens_out: u32) -> f64 {
    let (inp, outp) = price_per_million(model);
    (tokens_in as f64 * inp + tokens_out as f64 * outp) / 1_000_000.0
}

/// Cumulative usage for one provider.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ProviderUsage {
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_has_nonzero_price() {
        assert!(cost_usd("mock", 1000, 1000) > 0.0);
    }

    #[test]
    fn unknown_model_fails_closed_at_the_most_expensive_tier() {
        // A zero price here would let any unlisted model bypass budget_usd.
        let unknown = cost_usd("some-unlisted-model", 1_000_000, 1_000_000);
        assert_eq!(unknown, 15.00 + 75.00);
        // Fail-closed means: at least as expensive as everything in the table.
        for known in ["gpt-4", "claude-opus-4-8", "gpt-4o-mini"] {
            assert!(
                unknown >= cost_usd(known, 1_000_000, 1_000_000),
                "unknown must price >= {known}"
            );
        }
    }

    #[test]
    fn local_ollama_models_are_known_free() {
        assert_eq!(cost_usd("ollama/llama3.2", 1_000_000, 1_000_000), 0.0);
    }

    #[test]
    fn strips_provider_prefix_for_pricing() {
        assert_eq!(cost_usd("openai/gpt-4o", 1_000_000, 0), 2.50);
    }
}
