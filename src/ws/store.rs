//! Thread-safe map of active WebSocket connections. Lookups happen on every
//! message and on every `/_riz/connections/{id}` management call, so dashmap
//! gives us shard-locked O(1) without a global RwLock on the hot path.

use crate::ws::connection::{Connection, ConnectionId};
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct ConnectionStore {
    inner: Arc<DashMap<ConnectionId, Arc<Connection>>>,
}

impl ConnectionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, conn: Arc<Connection>) {
        self.inner.insert(conn.id.clone(), conn);
    }

    pub fn get(&self, id: &ConnectionId) -> Option<Arc<Connection>> {
        self.inner.get(id).map(|r| r.value().clone())
    }

    pub fn remove(&self, id: &ConnectionId) -> Option<Arc<Connection>> {
        self.inner.remove(id).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns a snapshot of all connections for the given function. Used by
    /// graceful shutdown to broadcast a close.
    pub fn by_function(&self, function_name: &str) -> Vec<Arc<Connection>> {
        self.inner
            .iter()
            .filter(|r| r.value().function_name == function_name)
            .map(|r| r.value().clone())
            .collect()
    }

    /// All connections, used by `kill_all_processes` on shutdown.
    pub fn all(&self) -> Vec<Arc<Connection>> {
        self.inner.iter().map(|r| r.value().clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::connection::OutboundMessage;
    use tokio::sync::{mpsc, oneshot};

    fn fake_conn(id: &str, function: &str) -> Arc<Connection> {
        let (tx, _rx) = mpsc::unbounded_channel::<OutboundMessage>();
        let (close_tx, _close_rx) = oneshot::channel();
        Arc::new(Connection {
            id: ConnectionId(id.into()),
            function_name: function.into(),
            connected_at: std::time::Instant::now(),
            last_active: std::sync::Mutex::new(std::time::Instant::now()),
            outbound: tx,
            close_signal: std::sync::Mutex::new(Some(close_tx)),
        })
    }

    #[test]
    fn insert_then_get_returns_same_arc() {
        let store = ConnectionStore::new();
        let c = fake_conn("c1", "chat");
        store.insert(c.clone());
        let got = store.get(&ConnectionId("c1".into())).unwrap();
        assert_eq!(got.id, c.id);
    }

    #[test]
    fn remove_returns_the_connection_and_drops_it() {
        let store = ConnectionStore::new();
        store.insert(fake_conn("c1", "chat"));
        assert_eq!(store.len(), 1);
        let removed = store.remove(&ConnectionId("c1".into())).unwrap();
        assert_eq!(removed.id.as_str(), "c1");
        assert!(store.is_empty());
        assert!(store.get(&ConnectionId("c1".into())).is_none());
    }

    #[test]
    fn by_function_filters_correctly() {
        let store = ConnectionStore::new();
        store.insert(fake_conn("c1", "chat"));
        store.insert(fake_conn("c2", "chat"));
        store.insert(fake_conn("c3", "notifications"));
        assert_eq!(store.by_function("chat").len(), 2);
        assert_eq!(store.by_function("notifications").len(), 1);
        assert_eq!(store.by_function("missing").len(), 0);
    }
}
