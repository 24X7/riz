//! Per-connection state for WebSocket APIs.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// Opaque connection identifier — AWS uses a base64-ish string; riz uses a
/// UUID v4 stringified, surfaced as `event.requestContext.connectionId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(pub String);

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ConnectionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ConnectionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for ConnectionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Message sent from the runtime to a connected client. `Close` triggers a
/// clean WebSocket close frame and removal from the connection store.
#[derive(Debug)]
pub enum OutboundMessage {
    Text(String),
    #[allow(dead_code)]
    Binary(Vec<u8>),
    Close,
}

/// Capacity of each connection's bounded outbound queue (Power of 10 rule 3,
/// docs/SAFETY.md: no unbounded growth behind a slow consumer). The writer
/// task drains at socket speed, so 256 frames absorbs normal bursts; when a
/// slow client lets the queue fill, `@connections` POST answers 429 — AWS
/// parity with PostToConnection's LimitExceededException — instead of
/// buffering without limit.
pub const OUTBOUND_CAPACITY: usize = 256;

/// Per-connection state held in the `ConnectionStore`. The writer task owns
/// the WebSocket sink and reads from `outbound_rx` to push messages.
pub struct Connection {
    pub id: ConnectionId,
    pub function_name: String,
    pub connected_at: Instant,
    pub last_active: std::sync::Mutex<Instant>,
    /// Outbound channel — anyone (incl. the management API) writes here to
    /// send a message to this client. Bounded ([`OUTBOUND_CAPACITY`]): when a
    /// slow client lets the queue fill, senders see an explicit `try_send`
    /// error (surfaced by the management API as 429) instead of growing the
    /// heap without limit.
    pub outbound: mpsc::Sender<OutboundMessage>,
    /// Fires when the connection is being torn down — readers and writer
    /// tasks watch this and exit. Take-once via [`Connection::take_close_signal`].
    pub close_signal: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl Connection {
    pub fn touch(&self) {
        if let Ok(mut t) = self.last_active.lock() {
            *t = Instant::now();
        }
    }

    /// Take the close signal sender, leaving `None` behind. Used by the
    /// connection-teardown path so the close frame is sent exactly once even
    /// if both the client and the management API try to close concurrently.
    pub fn take_close_signal(&self) -> Option<oneshot::Sender<()>> {
        self.close_signal.lock().ok().and_then(|mut g| g.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_id_is_unique_uuid_string() {
        let a = ConnectionId::new();
        let b = ConnectionId::new();
        assert_ne!(a, b);
        // UUID v4 = 36 chars
        assert_eq!(a.as_str().len(), 36);
        assert!(a.as_str().contains('-'));
    }

    #[test]
    fn connection_id_displays_as_inner_string() {
        let id = ConnectionId("abc-123".into());
        assert_eq!(format!("{id}"), "abc-123");
    }
}
