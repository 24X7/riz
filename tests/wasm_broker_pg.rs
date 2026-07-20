//! WASM resource broker v1 — the keystone e2e (harden backlog #4 proof).
//!
//! Spawns the REAL `riz run` binary with a `runtime = "wasm"` function that
//! holds a `[function.x.capabilities.db]` grant, backed by the in-process
//! Postgres wire mock. The guest (tests/fixtures/broker-wasm, built for
//! wasm32-wasip1) calls the `riz_capability.call` host import from inside
//! the WASI sandbox — it never opens a socket and never sees a DSN.
//!
//! Proves end-to-end, across the real process tree
//! (riz → pool child `riz __wasm-host` → wasmtime guest → broker → PG wire):
//!   * a granted guest runs a parameterized query and gets typed rows back;
//!   * deny-by-default: the same guest binary WITHOUT a grant gets a
//!     structured `denied` envelope, not data and not a trap;
//!   * a stalled backend is bounded by the grant's call_timeout — the guest
//!     gets `timeout`, the host is unaffected.
//!
//! SKIPS cleanly when the wasm32-wasip1 guest artifact isn't built
//! (`cargo build --release --target wasm32-wasip1` in tests/fixtures/broker-wasm).

#[path = "pg_wire_mock/mod.rs"]
mod pg_wire_mock;

use pg_wire_mock::MockPgServer;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn guest_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/broker-wasm/target/wasm32-wasip1/release/broker-wasm.wasm")
}

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if let Ok(resp) = reqwest::blocking::get(format!("http://127.0.0.1:{port}/ready")) {
            if resp.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn write_config(dir: &std::path::Path, port: u16) -> PathBuf {
    let wasm = guest_wasm();
    let toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[resources.pg.main]
dsn_env = "RIZ_PG_E2E_DSN"

# Granted: full envelope, generous timeout.
[function.db_orders]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 10000
concurrency = 1

[[function.db_orders.routes]]
path = "/db-orders"
method = "GET"

[function.db_orders.capabilities.db]
type = "pg"
resource = "pg.main"
call_timeout_ms = 5000

# Same guest binary, NO capabilities block — deny-by-default proof.
[function.db_denied]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 10000
concurrency = 1

[[function.db_denied.routes]]
path = "/db-denied"
method = "GET"

# Granted but with a tight call deadline — stalled-backend proof.
[function.db_slow]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 10000
concurrency = 1

[[function.db_slow.routes]]
path = "/db-slow"
method = "GET"

[function.db_slow.capabilities.db]
type = "pg"
resource = "pg.main"
call_timeout_ms = 300
"#,
        wasm = wasm.display(),
    );
    let path = dir.join("riz.toml");
    std::fs::write(&path, toml).expect("write config");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm_guest_brokers_pg_through_capability_grants() {
    if !guest_wasm().exists() {
        eprintln!(
            "wasm_broker_pg: guest not built — run `cargo build --release --target \
             wasm32-wasip1` in tests/fixtures/broker-wasm; skipping"
        );
        return;
    }

    let mock = MockPgServer::start().await;
    let dsn = mock.dsn();

    let port = pick_free_port();
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = write_config(dir.path(), port);

    // The REAL riz binary: main.rs hands [resources] to wasm pool children
    // via RIZ_BROKER_RESOURCES; the DSN env var rides the same inheritance.
    let child = Command::new(env!("CARGO_BIN_EXE_riz"))
        .args(["--config", cfg.to_str().unwrap(), "run"])
        .env("RIZ_PG_E2E_DSN", &dsn)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz run");
    let server = Server(child);

    let ready = tokio::task::spawn_blocking(move || wait_for_ready(port, Duration::from_secs(30)))
        .await
        .unwrap();
    assert!(ready, "riz never became ready");

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let client = reqwest::Client::new();

    // ── 1. Granted guest queries Postgres from inside the sandbox ────────
    let resp = client
        .get(format!("http://{addr}/db-orders"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "granted call must succeed: {text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["row_count"], 1, "{body}");
    assert_eq!(
        body["rows"][0]["id"], 1042,
        "typed int4 -> JSON number: {body}"
    );
    assert_eq!(body["rows"][0]["status"], "delayed", "{body}");

    // ── 2. Deny-by-default: same guest, no grant ──────────────────────────
    let resp = client
        .get(format!("http://{addr}/db-denied"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "denied call surfaces as guest 502");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(body["error"]["code"], "denied", "{body}");

    // ── 3. Stalled backend bounded by the grant deadline ─────────────────
    let started = std::time::Instant::now();
    let resp = client
        .get(format!("http://{addr}/db-slow"))
        .query(&[("sql", "select pg_sleep(600)")])
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "timeout", "{body}");
    assert!(
        elapsed < Duration::from_secs(5),
        "stall must be cut at ~300ms, took {elapsed:?}"
    );

    drop(server);
}
