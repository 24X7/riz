//! MCP Streamable HTTP transport (spec 2025-03-26 → 2025-11-25) for
//! `/_riz/mcp` — the axum-level layer that adds what the buffered Lambda
//! envelope can't carry:
//!
//! - **POST + `Accept: text/event-stream`** → the JSON-RPC response is
//!   delivered as an SSE `event: message` frame (the shape every Streamable
//!   HTTP client — MCP Inspector, Claude Code, Cursor, Cline — speaks).
//! - **GET + `Accept: text/event-stream`** → opens the server-initiated
//!   channel: a live SSE stream (comment heartbeats; riz currently has no
//!   unsolicited messages to push, the channel exists for spec conformance
//!   and future progress notifications).
//! - **DELETE** → explicit session termination (stateless server: 204).
//! - **`Mcp-Session-Id`** issued on `initialize`, echoed when supplied.
//!
//! Everything else (plain-JSON POST, GET without the SSE accept, OPTIONS
//! preflight) delegates verbatim to `dispatch_lambda`, preserving CORS,
//! access logs, bearer auth, and metrics exactly as before.

use axum::body::Body as AxumBody;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use futures_util::StreamExt;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::gateway::{
    ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
    ApiGatewayV2httpRequestContextHttpDescription,
};
use crate::state::AppState;

const SESSION_HEADER: &str = "mcp-session-id";

/// Single entry point registered as `any("/_riz/mcp")` in `build_app`.
pub async fn entry(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<AxumBody>,
) -> Response {
    let accept_sse = req
        .headers()
        .get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));

    match *req.method() {
        http::Method::GET if accept_sse => sse_get(state, req.headers().clone()).await,
        http::Method::DELETE => {
            // Stateless server: the session id is correlation-only, so
            // termination always succeeds. Echo the id back for symmetry.
            let mut resp = StatusCode::NO_CONTENT.into_response();
            if let Some(sid) = req.headers().get(SESSION_HEADER) {
                resp.headers_mut().insert(SESSION_HEADER, sid.clone());
            }
            resp
        }
        http::Method::POST if accept_sse => sse_post(state, peer, req).await,
        // Plain-JSON POST, non-SSE GET (405 contract), OPTIONS preflight:
        // the pre-existing buffered path, with all its middleware behavior.
        _ => crate::server::dispatch_lambda(State(state), ConnectInfo(peer), req).await,
    }
}

/// POST in SSE mode: dispatch the JSON-RPC body through the Router (bearer
/// auth, parsing, batching all live in McpHandler::invoke) and re-frame a
/// successful response as a one-event SSE stream. Errors (401 etc.) and
/// notification-only requests (202) pass through as plain HTTP — the spec
/// only streams successful JSON-RPC responses.
async fn sse_post(
    state: Arc<AppState>,
    peer: SocketAddr,
    req: Request<AxumBody>,
) -> Response {
    let started = std::time::Instant::now();
    let headers = req.headers().clone();
    let body_bytes = match axum::body::to_bytes(req.into_body(), 5 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    // Session correlation: echo a client-supplied id; mint one on initialize.
    let session_id = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            let is_initialize = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .is_some_and(|v| v.get("method").and_then(|m| m.as_str()) == Some("initialize"));
            is_initialize.then(|| uuid::Uuid::new_v4().to_string())
        });

    let event = make_mcp_event(&headers, &peer, body);
    let inner = {
        let router = state.router.read().await;
        match router.dispatch(event).await {
            Ok(outcome) => outcome.response,
            Err(e) => e.to_response(),
        }
    };
    // Mirror dispatch_lambda's metric so SSE-mode calls count as invocations.
    state
        .riz_state
        .record_invocation(
            "_riz_mcp",
            started.elapsed().as_secs_f64() * 1000.0,
            inner.status_code < 500,
            false,
        )
        .await;

    let body_text = |b: Option<aws_lambda_events::encodings::Body>| -> String {
        match b {
            Some(aws_lambda_events::encodings::Body::Text(s)) => s,
            Some(aws_lambda_events::encodings::Body::Binary(v)) => {
                String::from_utf8_lossy(&v).into_owned()
            }
            _ => String::new(),
        }
    };
    let mut resp = match inner.status_code {
        200 => {
            let payload = body_text(inner.body);
            let events =
                vec![Ok::<Event, Infallible>(Event::default().event("message").data(payload))];
            Sse::new(stream::iter(events)).into_response()
        }
        // McpHandler answers notification-only bodies with 202 Accepted
        // (Streamable HTTP spec) — no SSE frame to send.
        202 | 204 => StatusCode::ACCEPTED.into_response(),
        // 401 and other failures: plain HTTP so the client sees the status.
        status => {
            let code = StatusCode::from_u16(status as u16).unwrap_or(StatusCode::BAD_GATEWAY);
            (
                code,
                [("content-type", "application/json")],
                body_text(inner.body),
            )
                .into_response()
        }
    };
    if let Some(sid) = session_id {
        if let Ok(v) = http::HeaderValue::from_str(&sid) {
            resp.headers_mut().insert(SESSION_HEADER, v);
        }
    }
    resp
}

/// GET in SSE mode: the server-initiated channel. Riz is request/response —
/// there are no unsolicited server messages today — so the stream opens with
/// a comment frame and stays alive on heartbeats until the client hangs up.
/// Bearer-gated exactly like POST.
async fn sse_get(state: Arc<AppState>, headers: HeaderMap) -> Response {
    let expected = { state.config.read().await.effective_bearer_token() };
    if let Some(expected) = expected {
        let auth = headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if !crate::auth::bearer::validate_bearer(auth, &expected) {
            return (
                StatusCode::UNAUTHORIZED,
                [("content-type", "application/json")],
                r#"{"error":"unauthorized"}"#,
            )
                .into_response();
        }
    }
    let opener = stream::once(async {
        Ok::<Event, Infallible>(Event::default().comment(
            "mcp stream open — no server-initiated messages yet; channel held for notifications",
        ))
    });
    let mut resp = Sse::new(opener.chain(stream::pending()))
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response();
    if let Some(sid) = headers.get(SESSION_HEADER) {
        resp.headers_mut().insert(SESSION_HEADER, sid.clone());
    }
    resp
}

/// Minimal AWS v2 event for the Router → McpHandler::invoke path. Only the
/// fields invoke actually reads (method, headers for auth, body) plus the
/// routing context need to be real.
fn make_mcp_event(
    headers: &HeaderMap,
    peer: &SocketAddr,
    body: String,
) -> ApiGatewayV2httpRequest {
    let route_key = "POST /_riz/mcp".to_string();
    let ctx = ApiGatewayV2httpRequestContext {
        route_key: Some(route_key.clone()),
        account_id: Some("riz".into()),
        stage: Some("$default".into()),
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        time_epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        http: ApiGatewayV2httpRequestContextHttpDescription {
            method: http::Method::POST,
            path: Some("/_riz/mcp".into()),
            protocol: Some("HTTP/1.1".into()),
            source_ip: Some(peer.ip().to_string()),
            user_agent: None,
        },
        ..Default::default()
    };
    ApiGatewayV2httpRequest {
        version: Some("2.0".into()),
        route_key: Some(route_key),
        raw_path: Some("/_riz/mcp".into()),
        raw_query_string: Some(String::new()),
        cookies: None,
        headers: headers.clone(),
        query_string_parameters: Default::default(),
        path_parameters: Default::default(),
        request_context: ctx,
        stage_variables: Default::default(),
        body: Some(body),
        is_base64_encoded: false,
        kind: None,
        method_arn: None,
        http_method: http::Method::POST,
        identity_source: None,
        authorization_token: None,
        resource: None,
    }
}
