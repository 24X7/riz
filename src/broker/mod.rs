//! WASM resource broker — host-mediated capability access.
//!
//! Design: `docs/superpowers/specs/2026-06-10-wasm-resource-broker-design.md`.
//! One sentence: **the host owns the blast radius.** A WASI guest never opens
//! a socket; it asks the host through a named capability grant, and the host
//! performs the I/O under strict limits and hands back bytes.
//!
//! This module is the *single dispatcher seam* every brokered verb funnels
//! through. All resiliency controls live here, in one place, in order:
//!
//! 1. **Deny-by-default** — the grant must exist and match the verb's type.
//! 2. **Request payload cap** — checked before any backend work.
//! 3. **Rate limit** — token bucket per grant; excess → `throttled`.
//! 4. **Concurrency cap** — semaphore per grant; excess → `throttled`
//!    (rejected, never queued — a guest can't stall the host by piling up).
//! 5. **Per-call deadline** — the backend I/O races a timeout → `timeout`.
//! 6. **Response payload cap** — checked before bytes reach the guest.
//! 7. **Audit** — grant, outcome, byte counts, latency traced per call.
//!
//! The wire shape (JSON in guest linear memory, mirroring the stdin/stdout
//! envelope guests already speak):
//!
//! ```json
//! // request            // success                  // failure
//! {"sql": "...",        {"ok": true,                {"ok": false,
//!  "params": [..]}       "rows": [{..}, ..],         "error": {"code": "denied",
//!                        "row_count": 2}                       "message": "..."}}
//! ```
//!
//! Error codes are a closed set the guest can match on: `denied`,
//! `throttled`, `timeout`, `too_large`, `bad_request`, `backend`.

pub mod client;
pub mod dynamo;
pub mod http;
pub mod pg;
pub mod server;
pub mod wire;

use crate::config::CapabilityGrant;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Backend behind a `pg`-type grant. The real implementation speaks
/// Postgres wire (see `pg`); tests substitute mocks to prove the envelope.
#[async_trait::async_trait]
pub trait PgBackend: Send + Sync {
    /// Run one parameterized query. `read_only` queries MUST be executed in
    /// a read-only transaction by the implementation.
    async fn query(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        read_only: bool,
    ) -> Result<PgRows, String>;
}

/// Rows in wire shape: one JSON object per row, column name → JSON value.
#[derive(Debug, Clone, Default)]
pub struct PgRows {
    pub rows: Vec<serde_json::Value>,
}

/// The backend behind a grant, one variant per capability class. The limit
/// envelope (deny/caps/rate/inflight/deadline/response-cap/audit) is shared;
/// only this inner call differs by verb.
pub enum GrantBackend {
    Pg(Arc<dyn PgBackend>),
    Http(Arc<http::HttpBackend>),
    Dynamo(Arc<dynamo::DynamoBackend>),
}

/// A grant armed with its runtime limit state.
struct GrantRuntime {
    cfg: CapabilityGrant,
    backend: GrantBackend,
    /// Concurrency cap. try_acquire only — never queue.
    inflight: Arc<tokio::sync::Semaphore>,
    /// Token bucket for rate_per_sec (None → unlimited).
    bucket: Option<tokio::sync::Mutex<TokenBucket>>,
}

struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: std::time::Instant,
}

impl TokenBucket {
    fn new(rate: u32) -> Self {
        Self {
            capacity: rate as f64,
            tokens: rate as f64,
            refill_per_sec: rate as f64,
            last: std::time::Instant::now(),
        }
    }
    fn try_take(&mut self) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The broker for one function instance: its grants, armed with limits.
pub struct Broker {
    grants: HashMap<String, GrantRuntime>,
}

/// Closed error-code set the guest can match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Denied,
    Throttled,
    Timeout,
    TooLarge,
    BadRequest,
    Backend,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Denied => "denied",
            ErrorCode::Throttled => "throttled",
            ErrorCode::Timeout => "timeout",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Backend => "backend",
        }
    }
}

fn error_bytes(code: ErrorCode, message: &str) -> Vec<u8> {
    serde_json::json!({
        "ok": false,
        "error": { "code": code.as_str(), "message": message }
    })
    .to_string()
    .into_bytes()
}

#[derive(serde::Deserialize)]
struct PgQueryRequest {
    sql: String,
    #[serde(default)]
    params: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct HttpFetchRequest {
    #[serde(default = "default_http_method")]
    method: String,
    path: String,
    #[serde(default)]
    body: Option<String>,
}

fn default_http_method() -> String {
    "GET".to_string()
}

impl Broker {
    /// Build a broker for one function: each grant is armed with its backend
    /// and fresh limit state. `backends` maps grant name → backend (the
    /// caller resolves `[resources.*]` and owns credentials — they never
    /// enter this module's data model).
    pub fn from_backends(
        grants: &indexmap::IndexMap<String, CapabilityGrant>,
        mut backends: HashMap<String, GrantBackend>,
    ) -> Self {
        let grants = grants
            .iter()
            .filter_map(|(name, cfg)| {
                let backend = backends.remove(name)?;
                Some((
                    name.clone(),
                    GrantRuntime {
                        inflight: Arc::new(tokio::sync::Semaphore::new(cfg.max_inflight as usize)),
                        bucket: cfg
                            .rate_per_sec
                            .map(|r| tokio::sync::Mutex::new(TokenBucket::new(r))),
                        cfg: cfg.clone(),
                        backend,
                    },
                ))
            })
            .collect();
        Self { grants }
    }

    /// The single dispatcher every brokered verb funnels through. Runs the
    /// shared limit envelope (deny / request cap / rate / inflight / deadline
    /// / response cap / audit) around the verb-specific backend call, and
    /// ALWAYS returns response bytes — the guest never sees a transport-level
    /// failure. Unknown verbs answer the closed-set `bad_request` envelope.
    pub async fn dispatch(&self, verb: &str, grant_name: &str, request: &[u8]) -> Vec<u8> {
        let started = std::time::Instant::now();
        let outcome = self.dispatch_inner(verb, grant_name, request).await;
        let (bytes, code) = match outcome {
            Ok(bytes) => (bytes, "ok"),
            Err((code, msg)) => (error_bytes(code, &msg), code.as_str()),
        };
        // Audit point: every brokered call, success or failure.
        tracing::info!(
            target: "riz::broker",
            grant = grant_name,
            verb = verb,
            outcome = code,
            request_bytes = request.len(),
            response_bytes = bytes.len(),
            latency_ms = started.elapsed().as_secs_f64() * 1000.0,
            "brokered call"
        );
        bytes
    }

    async fn dispatch_inner(
        &self,
        verb: &str,
        grant_name: &str,
        request: &[u8],
    ) -> Result<Vec<u8>, (ErrorCode, String)> {
        // 1. Deny-by-default: the grant must exist and its type must match the
        //    verb's capability class.
        let grant = self.grants.get(grant_name).ok_or_else(|| {
            (
                ErrorCode::Denied,
                format!("no capability grant named '{grant_name}'"),
            )
        })?;
        let expected_type = match verb {
            "pg.query" => "pg",
            "http.fetch" => "http",
            "dynamo.get_item" | "dynamo.put_item" | "dynamo.query" | "dynamo.delete_item" => {
                "dynamo"
            }
            other => {
                return Err((
                    ErrorCode::BadRequest,
                    format!(
                        "unknown capability verb {other:?} — this riz knows: pg.query, \
                         http.fetch, dynamo.get_item/put_item/query/delete_item"
                    ),
                ))
            }
        };
        if grant.cfg.r#type != expected_type {
            return Err((
                ErrorCode::Denied,
                format!(
                    "grant '{grant_name}' is type '{}', not '{expected_type}' (verb '{verb}')",
                    grant.cfg.r#type
                ),
            ));
        }
        // 2. Request cap — before any parsing or backend work.
        if request.len() > grant.cfg.max_request_bytes {
            return Err((
                ErrorCode::TooLarge,
                format!(
                    "request is {} bytes; grant '{grant_name}' caps requests at {}",
                    request.len(),
                    grant.cfg.max_request_bytes
                ),
            ));
        }
        // 3. Rate limit.
        if let Some(bucket) = &grant.bucket {
            if !bucket.lock().await.try_take() {
                return Err((
                    ErrorCode::Throttled,
                    format!(
                        "grant '{grant_name}' rate limit ({}/s) exceeded",
                        grant.cfg.rate_per_sec.unwrap_or_default()
                    ),
                ));
            }
        }
        // 4. Concurrency cap — reject, never queue.
        let _permit = grant.inflight.clone().try_acquire_owned().map_err(|_| {
            (
                ErrorCode::Throttled,
                format!(
                    "grant '{grant_name}' has {} calls in flight (max_inflight)",
                    grant.cfg.max_inflight
                ),
            )
        })?;
        // 5. Per-call deadline around the verb-specific backend I/O.
        let deadline = Duration::from_millis(grant.cfg.call_timeout_ms);
        let rows = tokio::time::timeout(deadline, self.run_verb(verb, grant, request))
            .await
            .map_err(|_| {
                (
                    ErrorCode::Timeout,
                    format!(
                        "backend did not answer within {}ms (grant '{grant_name}')",
                        grant.cfg.call_timeout_ms
                    ),
                )
            })??;
        // 6. Response cap — before bytes reach the guest.
        let body = serde_json::json!({
            "ok": true,
            "rows": rows.rows,
            "row_count": rows.rows.len(),
        })
        .to_string()
        .into_bytes();
        if body.len() > grant.cfg.max_response_bytes {
            return Err((
                ErrorCode::TooLarge,
                format!(
                    "response is {} bytes; grant '{grant_name}' caps responses at {}",
                    body.len(),
                    grant.cfg.max_response_bytes
                ),
            ));
        }
        Ok(body)
    }

    /// The verb-specific backend call, run under the shared deadline. Parses
    /// the type's request shape and returns rows (pg) or a single
    /// `{status,headers,body}` row (http) — both flow through the same
    /// response envelope above.
    async fn run_verb(
        &self,
        verb: &str,
        grant: &GrantRuntime,
        request: &[u8],
    ) -> Result<PgRows, (ErrorCode, String)> {
        match (verb, &grant.backend) {
            ("pg.query", GrantBackend::Pg(backend)) => {
                let req: PgQueryRequest = serde_json::from_slice(request)
                    .map_err(|e| (ErrorCode::BadRequest, format!("malformed request: {e}")))?;
                let read_only = grant.cfg.mode == "read-only";
                backend
                    .query(&req.sql, &req.params, read_only)
                    .await
                    .map_err(|e| (ErrorCode::Backend, e))
            }
            ("http.fetch", GrantBackend::Http(backend)) => {
                let req: HttpFetchRequest = serde_json::from_slice(request)
                    .map_err(|e| (ErrorCode::BadRequest, format!("malformed request: {e}")))?;
                let allowed = allowed_methods(&grant.cfg);
                backend
                    .fetch(&req.method, &req.path, &allowed, req.body.as_deref())
                    .await
                    .map_err(|e| (ErrorCode::Backend, e))
            }
            (v, GrantBackend::Dynamo(backend)) if v.starts_with("dynamo.") => {
                let read_only = grant.cfg.mode == "read-only";
                backend
                    .call(v, request, read_only, grant.cfg.key_prefix.as_deref())
                    .await
                    .map_err(|e| (ErrorCode::Backend, e))
            }
            // A grant/verb-class mismatch was already rejected in
            // dispatch_inner; this arm keeps the match total and fails closed.
            _ => Err((
                ErrorCode::BadRequest,
                format!("grant backend does not serve verb '{verb}'"),
            )),
        }
    }
}

/// Build the grant-name → backend map for one function from validated config,
/// one backend per grant routed by its capability type. Resolves every
/// referenced resource's credentials host-side (fail-fast); credentials never
/// enter this module's data model or cross to a guest.
pub fn backends_for_function(
    grants: &indexmap::IndexMap<String, CapabilityGrant>,
    resources: &crate::config::ResourcesConfig,
) -> Result<HashMap<String, GrantBackend>, String> {
    let mut map: HashMap<String, GrantBackend> = HashMap::new();
    for (gname, grant) in grants {
        let Some((_, rname)) = grant.resource.split_once('.') else {
            return Err(format!(
                "grant '{gname}': malformed resource '{}'",
                grant.resource
            ));
        };
        let backend = match grant.r#type.as_str() {
            "pg" => {
                let res = resources.pg.get(rname).ok_or_else(|| {
                    format!("grant '{gname}': unknown resource '{}'", grant.resource)
                })?;
                GrantBackend::Pg(Arc::new(pg::TokioPgBackend::from_resource(res)?))
            }
            "http" => {
                let res = resources.http.get(rname).ok_or_else(|| {
                    format!("grant '{gname}': unknown resource '{}'", grant.resource)
                })?;
                GrantBackend::Http(Arc::new(http::HttpBackend::from_resource(res)?))
            }
            "dynamo" => {
                let res = resources.dynamo.get(rname).ok_or_else(|| {
                    format!("grant '{gname}': unknown resource '{}'", grant.resource)
                })?;
                GrantBackend::Dynamo(Arc::new(dynamo::DynamoBackend::from_resource(res)?))
            }
            other => {
                return Err(format!(
                    "grant '{gname}': unknown capability type '{other}'"
                ))
            }
        };
        map.insert(gname.clone(), backend);
    }
    Ok(map)
}

/// The HTTP methods an `http` grant may use: its `methods` list, or `GET` when
/// empty; `read-only` mode forces `GET` regardless.
fn allowed_methods(cfg: &CapabilityGrant) -> Vec<String> {
    if cfg.mode == "read-only" {
        return vec!["GET".to_string()];
    }
    if cfg.methods.is_empty() {
        return vec!["GET".to_string()];
    }
    cfg.methods.clone()
}
