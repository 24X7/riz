//! `@connections` REST management API — mirrors AWS API Gateway's
//! Management API for WebSocket. Handlers call these endpoints (typically
//! via internal HTTP) to push messages to connected clients.
//!
//! - GET    /_riz/connections                  → list all live connections
//! - GET    /_riz/connections/{connectionId}  → connection info
//! - POST   /_riz/connections/{connectionId}  → send message (body = payload)
//! - DELETE /_riz/connections/{connectionId}  → disconnect

use async_trait::async_trait;

use crate::auth::bearer::validate_bearer;
use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};
use crate::runtime::{
    error_response,
    response::{empty_response, json_response},
    HandlerError, LambdaHandler, RouteEntry, RouteMethod,
};
use crate::ws::connection::{ConnectionId, OutboundMessage};
use crate::ws::ConnectionStore;

pub struct ConnectionsHandler {
    routes: Vec<RouteEntry>,
    connections: ConnectionStore,
    bearer_token: Option<String>,
}

impl ConnectionsHandler {
    pub fn new(connections: ConnectionStore, bearer_token: Option<String>) -> Self {
        Self {
            // Mount the list endpoint plus three per-connection methods.
            // The router first-matches by method so all live in this handler.
            routes: vec![
                RouteEntry {
                    method: RouteMethod::Get,
                    path: "/_riz/connections".into(),
                },
                RouteEntry {
                    method: RouteMethod::Get,
                    path: "/_riz/connections/{id}".into(),
                },
                RouteEntry {
                    method: RouteMethod::Post,
                    path: "/_riz/connections/{id}".into(),
                },
                RouteEntry {
                    method: RouteMethod::Delete,
                    path: "/_riz/connections/{id}".into(),
                },
            ],
            connections,
            bearer_token,
        }
    }
}

#[async_trait]
impl LambdaHandler for ConnectionsHandler {
    fn name(&self) -> &str {
        "_riz_connections"
    }
    fn routes(&self) -> &[RouteEntry] {
        &self.routes
    }

    async fn invoke(
        &self,
        event: ApiGatewayV2httpRequest,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        if let Some(expected) = &self.bearer_token {
            let auth_header = event
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            if !validate_bearer(auth_header, expected) {
                let path = event.raw_path.as_deref().unwrap_or("/_riz/connections");
                let ip = event
                    .request_context
                    .http
                    .source_ip
                    .as_deref()
                    .unwrap_or("-");
                tracing::warn!(path = %path, source_ip = %ip, "unauthorized request");
                return Ok(json_response(
                    401,
                    &serde_json::json!({"error": "unauthorized"}),
                ));
            }
        }
        let method = event.request_context.http.method.as_str().to_uppercase();

        // List endpoint: GET /_riz/connections (no path param).
        if event.path_parameters.get("id").is_none() {
            if method == "GET" {
                return self.list();
            }
            return Ok(error_response(405, &format!("method {method} not allowed")));
        }

        let id = event
            .path_parameters
            .get("id")
            .cloned()
            .ok_or_else(|| HandlerError::Internal("missing connectionId path param".into()))?;
        let conn_id = ConnectionId(id);

        match method.as_str() {
            "GET" => self.info(&conn_id),
            "POST" => self.post(&conn_id, event.body.as_deref().unwrap_or("")),
            "DELETE" => self.delete(&conn_id),
            other => Ok(error_response(405, &format!("method {other} not allowed"))),
        }
    }
}

impl ConnectionsHandler {
    fn list(&self) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let summaries: Vec<serde_json::Value> = self
            .connections
            .all()
            .iter()
            .map(|conn| {
                serde_json::json!({
                    "connectionId": conn.id.as_str(),
                    "function": conn.function_name,
                    "connectedAgeSecs": conn.connected_at.elapsed().as_secs(),
                })
            })
            .collect();
        Ok(json_response(200, &serde_json::json!(summaries)))
    }

    fn info(&self, id: &ConnectionId) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let Some(conn) = self.connections.get(id) else {
            return Ok(error_response(404, "connection not found"));
        };
        let connected_secs = conn.connected_at.elapsed().as_secs();
        let body = serde_json::json!({
            "connectionId": conn.id.as_str(),
            "function": conn.function_name,
            "connectedAgeSecs": connected_secs,
        });
        Ok(json_response(200, &body))
    }

    fn post(
        &self,
        id: &ConnectionId,
        payload: &str,
    ) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let Some(conn) = self.connections.get(id) else {
            return Ok(error_response(410, "connection gone"));
        };
        if conn
            .outbound
            .send(OutboundMessage::Text(payload.to_string()))
            .is_err()
        {
            return Ok(error_response(410, "connection writer closed"));
        }
        Ok(empty_response(200))
    }

    fn delete(&self, id: &ConnectionId) -> Result<ApiGatewayV2httpResponse, HandlerError> {
        let Some(conn) = self.connections.get(id) else {
            return Ok(error_response(404, "connection not found"));
        };
        // Fire the reader-exit signal first so the reader task exits immediately
        // rather than waiting for the next client message (~1 RTT delay).
        if let Some(tx) = conn.take_close_signal() {
            let _ = tx.send(());
        }
        // Queue a Close frame so the writer sends a clean WebSocket CLOSE to
        // the client; the reader and writer now wind down in parallel.
        let _ = conn.outbound.send(OutboundMessage::Close);
        Ok(empty_response(204))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::make_event;
    use crate::ws::connection::Connection;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::{mpsc, oneshot};

    // Returns the store AND the receiver so the channel stays open for the
    // duration of the test (dropping _rx closes the channel and makes send()
    // return Err, causing POST to return 410 instead of 200).
    fn fake_store_with_conn(
        conn_id: &str,
    ) -> (
        ConnectionStore,
        mpsc::UnboundedReceiver<OutboundMessage>,
        oneshot::Receiver<()>,
    ) {
        let store = ConnectionStore::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (close_tx, close_rx) = oneshot::channel();
        store.insert(Arc::new(Connection {
            id: ConnectionId(conn_id.into()),
            function_name: "chat".into(),
            connected_at: Instant::now(),
            last_active: std::sync::Mutex::new(Instant::now()),
            outbound: tx,
            close_signal: std::sync::Mutex::new(Some(close_tx)),
        }));
        (store, rx, close_rx)
    }

    fn make_ev_with_auth(
        method: &str,
        path: &str,
        conn_id: &str,
        token: &str,
    ) -> crate::gateway::ApiGatewayV2httpRequest {
        let mut ev = make_event(method, path);
        ev.path_parameters.insert("id".into(), conn_id.into());
        ev.headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        ev
    }

    // ─── Auth tests (representative verb: GET) ─────────────────────────────

    #[tokio::test]
    async fn connections_returns_401_when_token_required_and_missing() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, Some("secret".into()));
        let mut ev = make_event("GET", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn connections_returns_401_when_token_required_and_wrong() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, Some("secret".into()));
        let ev = make_ev_with_auth("GET", "/_riz/connections/c1", "c1", "wrong");
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn connections_returns_200_when_token_required_and_correct() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, Some("secret".into()));
        let ev = make_ev_with_auth("GET", "/_riz/connections/c1", "c1", "secret");
        let resp = h.invoke(ev).await.unwrap();
        // connection exists → 200 info response
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn connections_returns_200_when_no_token_configured() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, None);
        let mut ev = make_event("GET", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    /// All three verbs (GET, POST, DELETE) must reject wrong tokens.
    #[tokio::test]
    async fn connections_post_rejects_wrong_token() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, Some("secret".into()));
        let ev = make_ev_with_auth("POST", "/_riz/connections/c1", "c1", "wrong");
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    #[tokio::test]
    async fn connections_delete_rejects_wrong_token() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, Some("secret".into()));
        let ev = make_ev_with_auth("DELETE", "/_riz/connections/c1", "c1", "wrong");
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 401);
    }

    // ─── Functional tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn get_unknown_connection_returns_404() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, None);
        let mut ev = make_event("GET", "/_riz/connections/missing");
        ev.path_parameters.insert("id".into(), "missing".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn post_to_known_connection_returns_200() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, None);
        let mut ev = make_event("POST", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        ev.body = Some("hello".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn delete_known_connection_returns_204() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store, None);
        let mut ev = make_event("DELETE", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 204);
    }

    /// DELETE must fire the oneshot close signal so the reader exits immediately,
    /// and must also enqueue OutboundMessage::Close so the writer sends a CLOSE frame.
    /// This validates the dual-path teardown introduced to fix the RTT delay bug.
    #[tokio::test]
    async fn delete_fires_close_signal_and_queues_close_frame() {
        let (store, mut outbound_rx, close_rx) = fake_store_with_conn("c2");
        let h = ConnectionsHandler::new(store, None);
        let mut ev = make_event("DELETE", "/_riz/connections/c2");
        ev.path_parameters.insert("id".into(), "c2".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 204);

        // The oneshot must have been fired — close_rx resolves immediately.
        close_rx
            .await
            .expect("DELETE must fire the reader-exit close signal");

        // The outbound channel must have a Close message queued for the writer.
        match outbound_rx.try_recv() {
            Ok(OutboundMessage::Close) => {}
            other => panic!("expected OutboundMessage::Close, got {other:?}"),
        }
    }
}
