//! Broker `pg_query` against a real Postgres wire conversation (the
//! in-process mock in tests/pg_wire_mock) — proves the tokio-postgres
//! backend end-to-end: startup + statement_timeout, typed column → JSON
//! mapping, text param binding, read-only transaction wrapping, and the
//! broker deadline bounding a stalled backend. No Docker, no real PG.

#[path = "pg_wire_mock/mod.rs"]
mod pg_wire_mock;

use pg_wire_mock::MockPgServer;
use riz::broker::pg::TokioPgBackend;
use riz::broker::{Broker, GrantBackend};
use riz::config::{CapabilityGrant, PgResourceConfig};
use std::collections::HashMap;
use std::sync::Arc;

fn grant(overrides: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
    let mut g: CapabilityGrant = toml::from_str(
        r#"
type = "pg"
resource = "pg.main"
"#,
    )
    .unwrap();
    overrides(&mut g);
    g
}

/// Broker with one "db" grant backed by the real wire driver pointed at the
/// mock server. `env_key` must be unique per test (env is process-global,
/// but nextest gives each test its own process).
fn broker_for(mock: &MockPgServer, env_key: &str, g: CapabilityGrant) -> Broker {
    std::env::set_var(env_key, mock.dsn());
    let res: PgResourceConfig = toml::from_str(&format!(
        r#"
dsn_env = "{env_key}"
"#
    ))
    .unwrap();
    let backend = TokioPgBackend::from_resource(&res).expect("dsn env is set");
    let mut grants = indexmap::IndexMap::new();
    grants.insert("db".to_string(), g);
    let mut backends: HashMap<String, GrantBackend> = HashMap::new();
    backends.insert("db".to_string(), GrantBackend::Pg(Arc::new(backend)));
    Broker::from_backends(&grants, backends)
}

fn req(sql: &str, params: serde_json::Value) -> Vec<u8> {
    serde_json::json!({"sql": sql, "params": params})
        .to_string()
        .into_bytes()
}

fn parse(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test]
async fn pg_query_round_trips_typed_rows_through_the_wire() {
    let mock = MockPgServer::start().await;
    let b = broker_for(&mock, "RIZ_TEST_PG_DSN_ROUNDTRIP", grant(|_| {}));
    let out = parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select id, status from orders", serde_json::json!([])),
        )
        .await,
    );
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["row_count"], 1);
    // int4 column arrives as a JSON number, text as a string.
    assert_eq!(out["rows"][0]["id"], 1042, "{out}");
    assert_eq!(out["rows"][0]["status"], "delayed");
}

#[tokio::test]
async fn statement_timeout_is_applied_on_connect() {
    let mock = MockPgServer::start().await;
    let b = broker_for(&mock, "RIZ_TEST_PG_DSN_STMT_TO", grant(|_| {}));
    parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select id, status from orders", serde_json::json!([])),
        )
        .await,
    );
    assert!(
        mock.log_contains("SET statement_timeout = 2000"),
        "{:?}",
        mock.log.lock().unwrap()
    );
}

#[tokio::test]
async fn params_bind_as_text() {
    let mock = MockPgServer::start().await;
    let b = broker_for(&mock, "RIZ_TEST_PG_DSN_PARAMS", grant(|_| {}));
    let out = parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select $1::text as echo", serde_json::json!(["hello-1042"])),
        )
        .await,
    );
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["rows"][0]["echo"], "hello-1042");
    assert!(
        mock.log_contains("bind: [\"hello-1042\"]"),
        "{:?}",
        mock.log.lock().unwrap()
    );
}

#[tokio::test]
async fn numbers_and_nulls_bind_as_text_and_sql_null() {
    let mock = MockPgServer::start().await;
    let b = broker_for(&mock, "RIZ_TEST_PG_DSN_NULLS", grant(|_| {}));
    let out = parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select $1::text as echo", serde_json::json!([42])),
        )
        .await,
    );
    assert_eq!(out["rows"][0]["echo"], "42", "{out}");
    parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select $1::text as echo", serde_json::json!([null])),
        )
        .await,
    );
    assert!(
        mock.log_contains("bind: [\"NULL\"]"),
        "{:?}",
        mock.log.lock().unwrap()
    );
}

#[tokio::test]
async fn read_only_grant_wraps_queries_in_a_read_only_transaction() {
    let mock = MockPgServer::start().await;
    let b = broker_for(
        &mock,
        "RIZ_TEST_PG_DSN_RO",
        grant(|g| g.mode = "read-only".into()),
    );
    let out = parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select id, status from orders", serde_json::json!([])),
        )
        .await,
    );
    assert_eq!(out["ok"], true, "{out}");
    let log = mock.log.lock().unwrap().join("\n");
    let upper = log.to_uppercase();
    assert!(
        upper.contains("START TRANSACTION") || upper.contains("BEGIN"),
        "{log}"
    );
    assert!(upper.contains("READ ONLY"), "{log}");
    assert!(upper.contains("COMMIT"), "{log}");
}

#[tokio::test]
async fn stalled_backend_query_is_bounded_by_the_broker_deadline() {
    let mock = MockPgServer::start().await;
    let b = broker_for(
        &mock,
        "RIZ_TEST_PG_DSN_STALL",
        grant(|g| g.call_timeout_ms = 150),
    );
    let started = std::time::Instant::now();
    let out = parse(
        &b.dispatch(
            "pg.query",
            "db",
            &req("select pg_sleep(600)", serde_json::json!([])),
        )
        .await,
    );
    assert_eq!(out["error"]["code"], "timeout", "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "stall must be cut at ~150ms, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn missing_dsn_env_is_a_startup_error() {
    let res: PgResourceConfig = toml::from_str(
        r#"
dsn_env = "RIZ_TEST_PG_DSN_DEFINITELY_NOT_SET"
"#,
    )
    .unwrap();
    let err = match TokioPgBackend::from_resource(&res) {
        Ok(_) => panic!("missing env must be a startup error"),
        Err(e) => e,
    };
    assert!(err.contains("RIZ_TEST_PG_DSN_DEFINITELY_NOT_SET"), "{err}");
}
