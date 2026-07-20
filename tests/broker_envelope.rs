//! Broker resiliency envelope (WASM resource broker v1, design doc
//! 2026-06-10). Proves the single-dispatcher controls with mock backends:
//! deny-by-default, payload caps both directions, rate limit, concurrency
//! cap (rejected, never queued), per-call timeout (a stalled backend is
//! bounded, not host-affecting), read-only mode propagation, and the closed
//! error-code set on the wire.

use riz::broker::{Broker, GrantBackend, PgBackend, PgRows};
use riz::config::CapabilityGrant;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Mock backend: configurable delay + payload, records read_only flags.
struct MockPg {
    delay_ms: u64,
    rows: Vec<serde_json::Value>,
    saw_read_only: AtomicBool,
    calls: AtomicUsize,
}

impl MockPg {
    fn new(delay_ms: u64, rows: Vec<serde_json::Value>) -> Arc<Self> {
        Arc::new(Self {
            delay_ms,
            rows,
            saw_read_only: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl PgBackend for MockPg {
    async fn query(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
        read_only: bool,
    ) -> Result<PgRows, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.saw_read_only.store(read_only, Ordering::SeqCst);
        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }
        Ok(PgRows {
            rows: self.rows.clone(),
        })
    }
}

fn grant(overrides: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
    let mut g: CapabilityGrant = toml::from_str(
        r#"
type = "pg"
resource = "pg.main"
"#,
    )
    .expect("base grant parses with defaults");
    overrides(&mut g);
    g
}

fn broker_with(name: &str, g: CapabilityGrant, backend: Arc<dyn PgBackend>) -> Broker {
    let mut grants = indexmap::IndexMap::new();
    grants.insert(name.to_string(), g);
    let mut backends: HashMap<String, GrantBackend> = HashMap::new();
    backends.insert(name.to_string(), GrantBackend::Pg(backend));
    Broker::from_backends(&grants, backends)
}

fn req(sql: &str) -> Vec<u8> {
    serde_json::json!({"sql": sql, "params": []})
        .to_string()
        .into_bytes()
}

fn parse(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("broker always answers JSON")
}

// ───────────────────────────── happy path ────────────────────────────────

#[tokio::test]
async fn granted_query_round_trips_rows() {
    let rows = vec![serde_json::json!({"id": 1042, "status": "delayed"})];
    let b = broker_with("db", grant(|_| {}), MockPg::new(0, rows));
    let out = parse(&b.dispatch("pg.query", "db", &req("select 1")).await);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["row_count"], 1);
    assert_eq!(out["rows"][0]["status"], "delayed");
}

// ───────────────────────────── deny-by-default ───────────────────────────

#[tokio::test]
async fn unknown_grant_is_denied() {
    let b = broker_with("db", grant(|_| {}), MockPg::new(0, vec![]));
    let out = parse(
        &b.dispatch("pg.query", "not-granted", &req("select 1"))
            .await,
    );
    assert_eq!(out["ok"], false);
    assert_eq!(out["error"]["code"], "denied", "{out}");
}

#[tokio::test]
async fn empty_broker_denies_everything() {
    let b = Broker::from_backends(&indexmap::IndexMap::new(), HashMap::new());
    let out = parse(&b.dispatch("pg.query", "db", &req("select 1")).await);
    assert_eq!(out["error"]["code"], "denied", "{out}");
}

// ───────────────────────────── payload caps ──────────────────────────────

#[tokio::test]
async fn oversized_request_is_rejected_before_backend() {
    let backend = MockPg::new(0, vec![]);
    let b = broker_with("db", grant(|g| g.max_request_bytes = 64), backend.clone());
    let big_sql = "select ".to_string() + &"x".repeat(200);
    let out = parse(&b.dispatch("pg.query", "db", &req(&big_sql)).await);
    assert_eq!(out["error"]["code"], "too_large", "{out}");
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        0,
        "backend must never see an oversized request"
    );
}

#[tokio::test]
async fn oversized_response_is_capped_before_the_guest() {
    let huge = vec![serde_json::json!({"blob": "y".repeat(10_000)})];
    let b = broker_with(
        "db",
        grant(|g| g.max_response_bytes = 1024),
        MockPg::new(0, huge),
    );
    let out = parse(&b.dispatch("pg.query", "db", &req("select blob")).await);
    assert_eq!(out["error"]["code"], "too_large", "{out}");
}

// ───────────────────────────── deadline ──────────────────────────────────

#[tokio::test]
async fn stalled_backend_is_bounded_by_the_call_timeout() {
    let b = broker_with(
        "db",
        grant(|g| g.call_timeout_ms = 50),
        MockPg::new(5_000, vec![]),
    );
    let started = std::time::Instant::now();
    let out = parse(
        &b.dispatch("pg.query", "db", &req("select pg_sleep(5)"))
            .await,
    );
    let elapsed = started.elapsed();
    assert_eq!(out["error"]["code"], "timeout", "{out}");
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "a 5s stall must be cut at ~50ms, took {elapsed:?}"
    );
}

// ───────────────────────────── concurrency cap ───────────────────────────

#[tokio::test]
async fn second_inflight_call_is_throttled_not_queued() {
    let b = Arc::new(broker_with(
        "db",
        grant(|g| {
            g.max_inflight = 1;
            g.call_timeout_ms = 1_000;
        }),
        MockPg::new(300, vec![]),
    ));
    let b2 = b.clone();
    let slow =
        tokio::spawn(async move { parse(&b2.dispatch("pg.query", "db", &req("select 1")).await) });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let it occupy the permit
    let started = std::time::Instant::now();
    let out = parse(&b.dispatch("pg.query", "db", &req("select 2")).await);
    assert_eq!(out["error"]["code"], "throttled", "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "throttle must reject immediately, not queue"
    );
    assert_eq!(slow.await.unwrap()["ok"], true, "first call unaffected");
}

// ───────────────────────────── rate limit ────────────────────────────────

#[tokio::test]
async fn rate_limit_throttles_burst_beyond_bucket() {
    let b = broker_with(
        "db",
        grant(|g| g.rate_per_sec = Some(2)),
        MockPg::new(0, vec![]),
    );
    assert_eq!(
        parse(&b.dispatch("pg.query", "db", &req("a")).await)["ok"],
        true
    );
    assert_eq!(
        parse(&b.dispatch("pg.query", "db", &req("b")).await)["ok"],
        true
    );
    let third = parse(&b.dispatch("pg.query", "db", &req("c")).await);
    assert_eq!(third["error"]["code"], "throttled", "{third}");
}

// ───────────────────────────── modes & shape ─────────────────────────────

#[tokio::test]
async fn read_only_mode_reaches_the_backend() {
    let backend = MockPg::new(0, vec![]);
    let b = broker_with(
        "db",
        grant(|g| g.mode = "read-only".into()),
        backend.clone(),
    );
    parse(&b.dispatch("pg.query", "db", &req("select 1")).await);
    assert!(backend.saw_read_only.load(Ordering::SeqCst));
}

#[tokio::test]
async fn malformed_request_is_bad_request() {
    let b = broker_with("db", grant(|_| {}), MockPg::new(0, vec![]));
    let out = parse(&b.dispatch("pg.query", "db", b"not json").await);
    assert_eq!(out["error"]["code"], "bad_request", "{out}");
}

// ───────────────────────────── config validation ─────────────────────────

#[test]
fn grant_must_reference_a_declared_resource() {
    let toml_src = r#"
[function.orders]
runtime = "wasm"
handler = "./orders.wasm"

[function.orders.capabilities.db]
type = "pg"
resource = "pg.main"
"#;
    let cfg: riz::config::Config = toml::from_str(toml_src).unwrap();
    let err = cfg.validate().expect_err("undeclared resource must fail");
    assert!(err.contains("resources.pg.main"), "{err}");
}

#[test]
fn capabilities_are_wasm_only_in_v1() {
    let toml_src = r#"
[resources.pg.main]
dsn_env = "RIZ_PG_MAIN_DSN"

[function.api]
runtime = "bun"
handler = "./api.ts"

[function.api.capabilities.db]
type = "pg"
resource = "pg.main"
"#;
    let cfg: riz::config::Config = toml::from_str(toml_src).unwrap();
    let err = cfg.validate().expect_err("bun + capabilities must fail");
    assert!(err.contains("WASM-only"), "{err}");
}

#[test]
fn valid_grant_parses_with_spec_defaults() {
    let toml_src = r#"
[resources.pg.main]
dsn_env = "RIZ_PG_MAIN_DSN"

[function.orders]
runtime = "wasm"
handler = "./orders.wasm"
timeout_ms = 5000

[function.orders.capabilities.db]
type = "pg"
resource = "pg.main"
mode = "read-only"
rate_per_sec = 50
"#;
    let cfg: riz::config::Config = toml::from_str(toml_src).unwrap();
    cfg.validate().expect("validates");
    let g = &cfg.functions["orders"].capabilities["db"];
    assert_eq!(g.max_inflight, 4);
    assert_eq!(g.call_timeout_ms, 1500);
    assert_eq!(g.max_request_bytes, 64 * 1024);
    assert_eq!(g.max_response_bytes, 1024 * 1024);
    assert_eq!(g.mode, "read-only");
    assert_eq!(cfg.resources.pg["main"].statement_timeout_ms, 2000);
}

#[test]
fn call_timeout_must_fit_inside_function_timeout() {
    let toml_src = r#"
[resources.pg.main]
dsn_env = "RIZ_PG_MAIN_DSN"

[function.orders]
runtime = "wasm"
handler = "./orders.wasm"
timeout_ms = 1000

[function.orders.capabilities.db]
type = "pg"
resource = "pg.main"
call_timeout_ms = 5000
"#;
    let cfg: riz::config::Config = toml::from_str(toml_src).unwrap();
    let err = cfg.validate().expect_err("call timeout > fn timeout");
    assert!(err.contains("call_timeout_ms"), "{err}");
}
