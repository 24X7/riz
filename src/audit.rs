//! Structured audit log — a tamper-evident record of who deployed, reloaded
//! config, or was denied at an auth boundary.
//!
//! Events are emitted at INFO on the dedicated `riz.audit` tracing target, as
//! structured fields (not interpolated text), so an operator can route them
//! with a target filter (`RUST_LOG=riz.audit=info`) to a separate sink while
//! they still flow to the default log by default. No new sink infrastructure:
//! durable/remote audit sinks are fleet scope.
//!
//! These are low-volume *boundary* events — administrative actions and auth
//! denials — not per-request access logs (that is the dispatch log's job).
//! Every event is scrubbed of secret material: tokens, bearer values, deploy
//! keys, and API-key secrets are NEVER fields. The caller identity is an IP or
//! a configured caller name, never the credential it presented.

/// The tracing target every audit event carries. Filter on this to isolate the
/// audit stream.
pub const TARGET: &str = "riz.audit";

/// A deploy attempt reached the deploy handler. `principal` is the client IP;
/// `source` is the artifact location (e.g. `s3://bucket/key`); `outcome` is
/// one of `rejected` (auth/validation), `failed` (swap error), or `applied`.
/// The deploy key is never included.
pub fn deploy(principal: &str, lambda: &str, source: &str, outcome: &str) {
    tracing::info!(
        target: TARGET,
        event = "deploy",
        principal = principal,
        lambda = lambda,
        source = source,
        outcome = outcome,
    );
}

/// A config hot-reload was applied, with the shape of the change (counts only,
/// no config contents).
pub fn config_reload(added: usize, removed: usize, changed: usize) {
    tracing::info!(
        target: TARGET,
        event = "config_reload",
        added = added,
        removed = removed,
        changed = changed,
    );
}

/// A request was denied at an auth boundary. `boundary` names the gate
/// (`api_key`, `authorizer`); `principal` is the caller identity (source IP —
/// the denied credential is deliberately absent); `reason` is a short code
/// (`unknown_or_absent_key`, `unauthorized`, `forbidden`, `error`).
pub fn auth_denied(boundary: &str, principal: &str, reason: &str) {
    tracing::info!(
        target: TARGET,
        event = "auth_denied",
        boundary = boundary,
        principal = principal,
        reason = reason,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Layer;

    #[derive(Clone, Default, Debug)]
    struct Record {
        target: String,
        fields: BTreeMap<String, String>,
    }

    struct Collector<'a>(&'a mut BTreeMap<String, String>);
    impl Visit for Collector<'_> {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            // Fallback for any type the typed hooks above didn't catch. Trim the
            // Debug quotes so string assertions match cleanly.
            let s = format!("{value:?}");
            let s = s.trim_matches('"').to_string();
            self.0.insert(field.name().to_string(), s);
        }
    }

    struct CaptureLayer {
        events: Arc<Mutex<Vec<Record>>>,
    }
    impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut rec = Record {
                target: event.metadata().target().to_string(),
                fields: BTreeMap::new(),
            };
            event.record(&mut Collector(&mut rec.fields));
            if let Ok(mut guard) = self.events.lock() {
                guard.push(rec);
            }
        }
    }

    /// Run `f` with a capturing subscriber installed for this thread, and
    /// return only the events emitted on the `riz.audit` target.
    fn capture(f: impl FnOnce()) -> Vec<Record> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let layer = CaptureLayer {
            events: events.clone(),
        };
        let subscriber = tracing_subscriber::registry().with(layer);
        with_default(subscriber, f);
        let all = events.lock().unwrap().clone();
        all.into_iter().filter(|r| r.target == TARGET).collect()
    }

    #[test]
    fn deploy_event_has_fields_and_no_secret() {
        let events = capture(|| {
            deploy(
                "203.0.113.7",
                "checkout",
                "s3://artifacts/checkout.zip",
                "applied",
            );
        });
        assert_eq!(events.len(), 1, "one audit event on riz.audit");
        let f = &events[0].fields;
        assert_eq!(f.get("event").map(String::as_str), Some("deploy"));
        assert_eq!(f.get("principal").map(String::as_str), Some("203.0.113.7"));
        assert_eq!(f.get("lambda").map(String::as_str), Some("checkout"));
        assert_eq!(f.get("outcome").map(String::as_str), Some("applied"));
        // No secret material may ever appear.
        let joined = f.values().cloned().collect::<Vec<_>>().join("|");
        assert!(
            !joined.to_lowercase().contains("key") && !joined.contains("Bearer"),
            "audit event must not carry secret material: {joined}"
        );
    }

    #[test]
    fn auth_denied_event_carries_boundary_and_reason() {
        let events = capture(|| {
            auth_denied("api_key", "198.51.100.4", "unknown_or_absent_key");
        });
        assert_eq!(events.len(), 1);
        let f = &events[0].fields;
        assert_eq!(f.get("event").map(String::as_str), Some("auth_denied"));
        assert_eq!(f.get("boundary").map(String::as_str), Some("api_key"));
        assert_eq!(f.get("principal").map(String::as_str), Some("198.51.100.4"));
        assert_eq!(
            f.get("reason").map(String::as_str),
            Some("unknown_or_absent_key")
        );
    }

    #[test]
    fn config_reload_event_reports_counts() {
        let events = capture(|| {
            config_reload(2, 1, 3);
        });
        assert_eq!(events.len(), 1);
        let f = &events[0].fields;
        assert_eq!(f.get("event").map(String::as_str), Some("config_reload"));
        assert_eq!(f.get("added").map(String::as_str), Some("2"));
        assert_eq!(f.get("removed").map(String::as_str), Some("1"));
        assert_eq!(f.get("changed").map(String::as_str), Some("3"));
    }

    #[test]
    fn non_audit_events_are_not_captured_as_audit() {
        // A normal log on a different target must not pollute the audit stream.
        let events = capture(|| {
            tracing::info!(target: "riz.request", event = "not_audit");
        });
        assert!(events.is_empty(), "only riz.audit events are audit events");
    }
}
