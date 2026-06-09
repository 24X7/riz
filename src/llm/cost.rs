//! Per-call cost estimation from a static pricing table ($/token), and the
//! cumulative usage ledger the gateway exposes for AI-FinOps.
//!
//! Prices are illustrative USD per 1M tokens (input, output); refresh as
//! providers change them. Unknown models price at zero. The `mock` provider
//! carries a nominal price so cost/budget are demonstrable with no real API.

use serde::Serialize;

/// USD per 1M tokens, as (input, output).
fn price_per_million(model: &str) -> (f64, f64) {
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
        _ => (0.0, 0.0),
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
    fn mock_has_nonzero_price_unknown_is_free() {
        assert!(cost_usd("mock", 1000, 1000) > 0.0);
        assert_eq!(cost_usd("some-unlisted-model", 1000, 1000), 0.0);
    }

    #[test]
    fn strips_provider_prefix_for_pricing() {
        assert_eq!(cost_usd("openai/gpt-4o", 1_000_000, 0), 2.50);
    }
}
