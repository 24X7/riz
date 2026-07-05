//! WASM guards — pre/post-invoke policy modules (v1 roadmap #3/#4).
//!
//! A guard is a `wasm32-wasip1` module that rides the SAME pool machinery as
//! every handler: it's spawned as a `riz __wasm-host` child, watched by the
//! liveness loop, respawned on crash, and invoked over the line-delimited
//! JSON envelope. One guard protects every runtime alike — the polyglot
//! pool is what makes a cross-runtime safety layer possible at all.
//!
//! ## Verdict contract (what the guard module writes per input line)
//!
//! ```json
//! {"action": "allow"}                                  // pass through
//! {"action": "allow", "event": { ...mutated event }}    // scrubbed payload
//! {"action": "deny", "statusCode": 451, "body": "..."} // reject; handler never runs
//! ```
//!
//! For `guard_out` the same shape applies with `response` in place of
//! `event`: allow passes the handler's response through, `response` replaces
//! it (redaction), deny swaps in a status+body.
//!
//! **Failures fail CLOSED.** A guard that crashes, times out, emits garbage,
//! or has an unhealthy pool produces a 502 to the client — a configured
//! policy that can't run must never silently allow traffic.

use crate::config::FunctionConfig;
use serde::Deserialize;
use std::path::Path;

/// Pool-name suffix for a function's pre-invoke guard.
pub const GUARD_IN_SUFFIX: &str = "::guard_in";
/// Pool-name suffix for a function's post-invoke guard.
pub const GUARD_OUT_SUFFIX: &str = "::guard_out";
/// Per-call guard budget. Guards are policy, not business logic — they get
/// a tight, non-configurable deadline so a slow guard can't become a slow
/// API.
pub const GUARD_TIMEOUT_MS: u64 = 2_000;

/// The verdict a guard module answers with. `Default` (empty action) is
/// deliberately NOT a valid verdict — `ProcessManager::invoke_generic`
/// returns `R::default()` for an unhealthy pool, so an unreachable guard
/// parses as "action we don't understand" and fails closed for free.
#[derive(Debug, Default, Deserialize)]
pub struct GuardVerdict {
    #[serde(default)]
    pub action: String,
    /// `guard_in`: replacement event when the guard mutates the request.
    #[serde(default)]
    pub event: Option<serde_json::Value>,
    /// `guard_out`: replacement response envelope.
    #[serde(default)]
    pub response: Option<serde_json::Value>,
    /// Deny status (default 403).
    #[serde(default, rename = "statusCode")]
    pub status_code: Option<u16>,
    /// Deny body, verbatim.
    #[serde(default)]
    pub body: Option<String>,
}

/// A guard verdict AFTER the fail-closed gate: the only two actions that
/// exist for callers. Encoding the closed set in a type (rule 5) removes the
/// "unknown action" arm — and its `unreachable!` — from every call site;
/// garbage actions are rejected where the verdict is parsed.
#[derive(Debug)]
pub enum GuardDecision {
    /// Proceed; the verdict may carry a mutated `event` (guard_in) or a
    /// replacement `response` (guard_out).
    Allow(GuardVerdict),
    /// Reject with this status and body; the guarded stage never runs.
    Deny { status_code: u16, body: String },
}

/// Deny default status when the guard names none.
pub const DENY_DEFAULT_STATUS: u16 = 403;
/// Deny default body when the guard names none.
pub const DENY_DEFAULT_BODY: &str = r#"{"error":"rejected by guard"}"#;

/// Synthesize the pool config a guard runs under: always the WASM runtime,
/// the guard module as the handler, and the parent function's concurrency
/// (a guard must never be a tighter bottleneck than the handler it guards).
/// No capabilities, no nested guards — a guard is pure compute.
pub fn guard_pool_config(guard_module: &Path, base: &FunctionConfig) -> FunctionConfig {
    FunctionConfig {
        runtime: crate::config::RuntimeKind::Wasm,
        protocol: Default::default(),
        handler: guard_module.to_path_buf(),
        timeout_ms: GUARD_TIMEOUT_MS,
        integration_timeout_ms: GUARD_TIMEOUT_MS,
        stage_variables: Default::default(),
        env: Default::default(),
        cache_ttl_secs: None,
        concurrency: base.concurrency,
        routes: Vec::new(),
        cors: None,
        authorizer: None,
        memory_mb: None,
        cpu_time_secs: None,
        allowed_paths: None,
        mcp: None,
        capabilities: Default::default(),
        guard_in: None,
        guard_out: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_verdict_is_not_allow() {
        // The fail-closed keystone: an unhealthy guard pool yields
        // GuardVerdict::default(), whose action must never read as allow.
        let v = GuardVerdict::default();
        assert_ne!(v.action, "allow");
        assert_ne!(v.action, "deny");
    }

    #[test]
    fn verdict_parses_all_documented_shapes() {
        let allow: GuardVerdict = serde_json::from_str(r#"{"action":"allow"}"#).unwrap();
        assert_eq!(allow.action, "allow");
        let mutate: GuardVerdict =
            serde_json::from_str(r#"{"action":"allow","event":{"k":1}}"#).unwrap();
        assert!(mutate.event.is_some());
        let deny: GuardVerdict =
            serde_json::from_str(r#"{"action":"deny","statusCode":451,"body":"no"}"#).unwrap();
        assert_eq!(deny.status_code, Some(451));
        assert_eq!(deny.body.as_deref(), Some("no"));
    }
}
