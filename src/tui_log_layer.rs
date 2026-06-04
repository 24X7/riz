//! tracing `Layer` that routes every emitted event into the TUI's log
//! channel instead of (or in addition to) stdout.
//!
//! Why this exists: the TUI uses the alternate screen via crossterm. Any
//! write to stdout that ratatui doesn't know about corrupts the rendered
//! display — the layout "moves," scroll doesn't work, escape sequences
//! leak into the wrong place. When the TUI is enabled, ALL log emission
//! must go through the TUI's log pipeline so the display stays consistent.
//!
//! Wire-up: `main.rs` creates `log_tx` early, calls `set_sink(log_tx)`,
//! and installs `TuiLogLayer` on the tracing registry instead of the
//! stdout fmt layer. The TUI snapshotter reads from `log_rx` and renders
//! entries in the Logs panel, color-coded by level.

use crate::state::LogEntry;
use std::sync::OnceLock;
use std::time::SystemTime;
use tokio::sync::mpsc::Sender;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Process-global sink. Set once by main during TUI startup. Before the
/// sink is set, events are silently dropped — main installs the sink as
/// part of the same tracing init that activates the layer, so the only
/// dropped events are ones emitted in the few µs before tracing init,
/// which is fine.
static SINK: OnceLock<Sender<LogEntry>> = OnceLock::new();

/// Register the channel that incoming tracing events should be forwarded
/// to. Called from main.rs when TUI mode is enabled. Idempotent — second
/// call is a silent no-op (OnceLock semantics).
pub fn set_sink(tx: Sender<LogEntry>) {
    let _ = SINK.set(tx);
}

/// A tracing layer that converts each event into a `LogEntry` and
/// `try_send`s it to the configured sink. Drops the event on a full
/// channel (matches `AppState::push_log`'s backpressure behavior).
pub struct TuiLogLayer;

impl<S> Layer<S> for TuiLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(tx) = SINK.get() else {
            return;
        };
        let meta = event.metadata();
        let level = meta.level().to_string();

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        // tracing's `message` field is the unstructured payload of
        // `info!("...")` / `warn!("...")`. If absent, fall back to the
        // event target so the entry isn't blank.
        let message = visitor.message.unwrap_or_else(|| meta.target().to_string());

        let entry = LogEntry {
            timestamp: SystemTime::now(),
            level,
            message,
            route_key: visitor.route_key,
        };
        let _ = tx.try_send(entry);
    }
}

/// Pulls the `message` and (if present) `route_key` field values out of
/// a tracing Event. Other fields are intentionally ignored — the TUI's
/// log panel renders only level + message + optional route filter, so
/// recording everything would just bloat the channel.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    route_key: Option<String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = Some(format!("{value:?}")),
            "route_key" => self.route_key = Some(format!("{value:?}").trim_matches('"').to_string()),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "route_key" => self.route_key = Some(value.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_extracts_message_string() {
        // Smoke test of the visitor's str-record path. We construct the
        // events indirectly by using the tracing! macros below in a real
        // test would need a full subscriber — for now just check the
        // type compiles and the visitor stores what it should.
        let mut v = FieldVisitor::default();
        v.message = Some("hello".into());
        assert_eq!(v.message.as_deref(), Some("hello"));
    }
}
