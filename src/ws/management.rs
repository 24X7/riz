//! `@connections` REST management API — mirrors AWS API Gateway's
//! Management API for WebSocket. Handlers call these endpoints (typically
//! via internal HTTP) to push messages to connected clients.
//!
//! - GET    /_riz/connections/{connectionId}  → connection info
//! - POST   /_riz/connections/{connectionId}  → send message (body = payload)
//! - DELETE /_riz/connections/{connectionId}  → disconnect

use async_trait::async_trait;
use http::{header, HeaderMap, HeaderValue};

use crate::gateway::{ApiGatewayV2httpRequest, ApiGatewayV2httpResponse, Body};
use crate::runtime::{error_response, HandlerError, LambdaHandler, RouteEntry, RouteMethod};
use crate::ws::connection::{ConnectionId, OutboundMessage};
use crate::ws::ConnectionStore;

pub struct ConnectionsHandler {
    routes: Vec<RouteEntry>,
    connections: ConnectionStore,
}

impl ConnectionsHandler {
    pub fn new(connections: ConnectionStore) -> Self {
        Self {
            // Mount three routes — same path, three methods. The router
            // first-matches by method so all three live in this handler.
            routes: vec![
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
        let id = event
            .path_parameters
            .get("id")
            .cloned()
            .ok_or_else(|| HandlerError::Internal("missing connectionId path param".into()))?;
        let conn_id = ConnectionId(id);
        let method = event.request_context.http.method.as_str().to_uppercase();

        match method.as_str() {
            "GET" => self.info(&conn_id),
            "POST" => self.post(&conn_id, event.body.as_deref().unwrap_or("")),
            "DELETE" => self.delete(&conn_id),
            other => Ok(error_response(405, &format!("method {other} not allowed"))),
        }
    }
}

impl ConnectionsHandler {
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
        json_response(200, &body)
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
        let _ = conn.outbound.send(OutboundMessage::Close);
        Ok(empty_response(204))
    }
}

fn json_response(
    status: u16,
    value: &serde_json::Value,
) -> Result<ApiGatewayV2httpResponse, HandlerError> {
    let body = serde_json::to_string(value).map_err(|e| HandlerError::Internal(e.to_string()))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(ApiGatewayV2httpResponse {
        status_code: status as i64,
        headers,
        multi_value_headers: HeaderMap::new(),
        body: Some(Body::Text(body)),
        is_base64_encoded: false,
        cookies: Vec::new(),
    })
}

fn empty_response(status: u16) -> ApiGatewayV2httpResponse {
    ApiGatewayV2httpResponse {
        status_code: status as i64,
        headers: HeaderMap::new(),
        multi_value_headers: HeaderMap::new(),
        body: None,
        is_base64_encoded: false,
        cookies: Vec::new(),
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

    #[tokio::test]
    async fn get_unknown_connection_returns_404() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store);
        let mut ev = make_event("GET", "/_riz/connections/missing");
        ev.path_parameters.insert("id".into(), "missing".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn post_to_known_connection_returns_200() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store);
        let mut ev = make_event("POST", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        ev.body = Some("hello".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn delete_known_connection_returns_204() {
        let (store, _rx, _close_rx) = fake_store_with_conn("c1");
        let h = ConnectionsHandler::new(store);
        let mut ev = make_event("DELETE", "/_riz/connections/c1");
        ev.path_parameters.insert("id".into(), "c1".into());
        let resp = h.invoke(ev).await.unwrap();
        assert_eq!(resp.status_code, 204);
    }
}
