//! Per-worker **AWS Lambda Runtime API** endpoint — the `/2018-06-01/runtime/*`
//! HTTP contract (version 2018-06-01), implemented faithfully so that an
//! UNMODIFIED official compiled runtime client (Go's `github.com/aws/aws-lambda-go`
//! `lambda.Start`, Rust's `lambda_runtime::run`, or any `provided.al2023`
//! runtime) connects via `AWS_LAMBDA_RUNTIME_API` and runs with **no riz
//! library and no code change** — exactly as it would on AWS.
//!
//! One endpoint per worker SLOT: each riz worker is one AWS "execution
//! environment" handling one invocation at a time. The official clients carry
//! no worker id, so each worker connects to its own `127.0.0.1:port`. The
//! endpoint outlives individual child processes — a crashed child respawns and
//! re-polls the same port.
//!
//! Flow: the pool sends an [`Invocation`] on the endpoint's channel; the
//! worker's long-polling `GET …/invocation/next` receives it (event body +
//! `Lambda-Runtime-*` headers); the worker's `POST …/invocation/{id}/response`
//! (or `/error`) resolves the invocation's one-shot, which the pool is awaiting.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{mpsc, oneshot, Mutex};

/// Resolves to the worker's response bytes (`/response`) or an error string
/// (`/error`).
type ResponseTx = oneshot::Sender<Result<Vec<u8>, String>>;

/// One invocation handed to a worker through its Runtime-API endpoint.
pub struct Invocation {
    pub request_id: String,
    /// Unix-millis timeout (sent as `Lambda-Runtime-Deadline-Ms`).
    pub deadline_ms: i64,
    pub invoked_arn: String,
    /// The event JSON bytes (the body of `…/invocation/next`).
    pub event: Vec<u8>,
    /// The pool awaits this for the handler's response.
    pub respond: ResponseTx,
}

struct EndpointState {
    /// Pending invocations from the pool; `next` pops one (long-poll).
    invoke_rx: Mutex<mpsc::Receiver<Invocation>>,
    /// request_id → one-shot to resolve when `/response` or `/error` arrives.
    pending: Mutex<HashMap<String, ResponseTx>>,
}

/// A bound, running per-worker Runtime-API endpoint.
pub struct WorkerEndpoint {
    /// `127.0.0.1:port` to set as the child's `AWS_LAMBDA_RUNTIME_API`.
    pub addr: SocketAddr,
    /// Queue an invocation for this worker (capacity 1 — one in-flight).
    pub sender: mpsc::Sender<Invocation>,
    // The serve task; dropped (aborted) when the slot is torn down.
    _task: tokio::task::JoinHandle<()>,
}

impl WorkerEndpoint {
    /// Bind `127.0.0.1:0` and start serving the Runtime API.
    pub async fn start() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Invocation>(1);
        let state = Arc::new(EndpointState {
            invoke_rx: Mutex::new(rx),
            pending: Mutex::new(HashMap::new()),
        });
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            addr,
            sender: tx,
            _task: task,
        })
    }
}

impl Drop for WorkerEndpoint {
    fn drop(&mut self) {
        // Stop serving when the slot is gone (the child is killed separately).
        self._task.abort();
    }
}

fn build_router(state: Arc<EndpointState>) -> Router {
    Router::new()
        .route("/2018-06-01/runtime/invocation/next", get(next))
        .route(
            "/2018-06-01/runtime/invocation/:id/response",
            post(invocation_response),
        )
        .route(
            "/2018-06-01/runtime/invocation/:id/error",
            post(invocation_error),
        )
        .route("/2018-06-01/runtime/init/error", post(init_error))
        .with_state(state)
}

const H_REQUEST_ID: HeaderName = HeaderName::from_static("lambda-runtime-aws-request-id");
const H_DEADLINE: HeaderName = HeaderName::from_static("lambda-runtime-deadline-ms");
const H_ARN: HeaderName = HeaderName::from_static("lambda-runtime-invoked-function-arn");

/// `GET /2018-06-01/runtime/invocation/next` — long-poll for the next event.
/// NEVER times out (per the AWS contract): it awaits the pool's next send.
async fn next(State(state): State<Arc<EndpointState>>) -> Response {
    let inv = {
        let mut rx = state.invoke_rx.lock().await;
        rx.recv().await
    };
    let Some(inv) = inv else {
        // Channel closed: the slot is being torn down. A 500 tells a
        // well-behaved client to exit (the child is killed separately anyway).
        return (StatusCode::INTERNAL_SERVER_ERROR, "runtime shutting down").into_response();
    };

    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&inv.request_id) {
        headers.insert(H_REQUEST_ID, v);
    }
    if let Ok(v) = HeaderValue::from_str(&inv.deadline_ms.to_string()) {
        headers.insert(H_DEADLINE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&inv.invoked_arn) {
        headers.insert(H_ARN, v);
    }

    state
        .pending
        .lock()
        .await
        .insert(inv.request_id.clone(), inv.respond);

    (StatusCode::OK, headers, inv.event).into_response()
}

/// `POST /2018-06-01/runtime/invocation/{id}/response` — the handler's result.
async fn invocation_response(
    State(state): State<Arc<EndpointState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> StatusCode {
    if let Some(tx) = state.pending.lock().await.remove(&id) {
        let _ = tx.send(Ok(body.to_vec()));
    }
    StatusCode::ACCEPTED
}

/// `POST /2018-06-01/runtime/invocation/{id}/error` — a handler/runtime error.
async fn invocation_error(
    State(state): State<Arc<EndpointState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> StatusCode {
    if let Some(tx) = state.pending.lock().await.remove(&id) {
        let msg = String::from_utf8_lossy(&body).to_string();
        let _ = tx.send(Err(msg));
    }
    StatusCode::ACCEPTED
}

/// `POST /2018-06-01/runtime/init/error` — an init failure; log and accept.
async fn init_error(body: Bytes) -> StatusCode {
    tracing::warn!(
        "lambda runtime reported init error: {}",
        String::from_utf8_lossy(&body)
    );
    StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn api(addr: SocketAddr, path: &str) -> String {
        format!("http://{addr}{path}")
    }

    #[tokio::test]
    async fn next_delivers_event_and_runtime_headers() {
        let ep = WorkerEndpoint::start().await.unwrap();
        let (tx, _rx) = oneshot::channel();
        ep.sender
            .send(Invocation {
                request_id: "req-1".into(),
                deadline_ms: 1_700_000_000_000,
                invoked_arn: "arn:riz:lambda:local:000000000000:function:echo".into(),
                event: br#"{"rawPath":"/echo"}"#.to_vec(),
                respond: tx,
            })
            .await
            .unwrap();

        let resp = reqwest::get(api(ep.addr, "/2018-06-01/runtime/invocation/next"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("lambda-runtime-aws-request-id").unwrap(),
            "req-1"
        );
        assert_eq!(
            resp.headers().get("lambda-runtime-deadline-ms").unwrap(),
            "1700000000000"
        );
        assert!(resp
            .headers()
            .get("lambda-runtime-invoked-function-arn")
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(":function:echo"));
        assert_eq!(resp.text().await.unwrap(), r#"{"rawPath":"/echo"}"#);
    }

    #[tokio::test]
    async fn response_resolves_the_invocation_oneshot() {
        let ep = WorkerEndpoint::start().await.unwrap();
        let (tx, rx) = oneshot::channel();
        ep.sender
            .send(Invocation {
                request_id: "req-2".into(),
                deadline_ms: 1,
                invoked_arn: "arn".into(),
                event: b"{}".to_vec(),
                respond: tx,
            })
            .await
            .unwrap();

        // Drain `next` so the pending map records req-2.
        let _ = reqwest::get(api(ep.addr, "/2018-06-01/runtime/invocation/next"))
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let posted = client
            .post(api(
                ep.addr,
                "/2018-06-01/runtime/invocation/req-2/response",
            ))
            .body(r#"{"statusCode":200}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(posted.status(), 202);

        let got = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("oneshot resolved")
            .expect("sender not dropped");
        assert_eq!(got.unwrap(), br#"{"statusCode":200}"#);
    }

    #[tokio::test]
    async fn error_surfaces_as_err() {
        let ep = WorkerEndpoint::start().await.unwrap();
        let (tx, rx) = oneshot::channel();
        ep.sender
            .send(Invocation {
                request_id: "req-3".into(),
                deadline_ms: 1,
                invoked_arn: "arn".into(),
                event: b"{}".to_vec(),
                respond: tx,
            })
            .await
            .unwrap();
        let _ = reqwest::get(api(ep.addr, "/2018-06-01/runtime/invocation/next"))
            .await
            .unwrap();

        let client = reqwest::Client::new();
        client
            .post(api(ep.addr, "/2018-06-01/runtime/invocation/req-3/error"))
            .body(r#"{"errorMessage":"boom"}"#)
            .send()
            .await
            .unwrap();

        let got = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(got.unwrap_err().contains("boom"));
    }

    #[tokio::test]
    async fn unknown_request_id_is_a_noop_202() {
        let ep = WorkerEndpoint::start().await.unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .post(api(ep.addr, "/2018-06-01/runtime/invocation/nope/response"))
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
    }
}
