//! The per-grant concurrency cap is the "tombstone" guarantee: with the
//! broker in the daemon, `max_inflight` finally means per-function (it no
//! longer multiplies by `concurrency`). This proves the semaphore admits
//! exactly `max_inflight` concurrent calls and rejects the rest as
//! `throttled` — reject-not-queue, never a stall.
//!
//! In-process against a trait-mock backend that blocks on a barrier, so the
//! concurrency is deterministic without a real database.

use async_trait::async_trait;
use riz::broker::{Broker, PgBackend, PgRows};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;

/// A backend whose query blocks at `gate` (holding the grant's inflight
/// permit) until the test releases it, and records peak concurrency seen.
struct BlockingBackend {
    gate: Arc<Barrier>,
    peak: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}

#[async_trait]
impl PgBackend for BlockingBackend {
    async fn query(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
        _read_only: bool,
    ) -> Result<PgRows, String> {
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        self.gate.wait().await; // held until the test arrives at the barrier
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(PgRows {
            rows: vec![serde_json::json!({"one": 1})],
        })
    }
}

fn grant(max_inflight: u32) -> riz::config::CapabilityGrant {
    let mut g: riz::config::CapabilityGrant = toml::from_str(
        r#"
type = "pg"
resource = "pg.main"
"#,
    )
    .unwrap();
    g.max_inflight = max_inflight;
    g
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_inflight_admits_one_and_throttles_the_rest() {
    const ADMITTED: usize = 1;
    const CALLERS: usize = 4;

    // Barrier participants = the admitted call(s) + the test thread. The
    // admitted call blocks (holding the only permit) until the test arrives,
    // guaranteeing the other callers hit a full semaphore and are throttled.
    let gate = Arc::new(Barrier::new(ADMITTED + 1));
    let peak = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(BlockingBackend {
        gate: gate.clone(),
        peak: peak.clone(),
        live: Arc::new(AtomicUsize::new(0)),
    });

    let mut grants = indexmap::IndexMap::new();
    grants.insert("db".to_string(), grant(ADMITTED as u32));
    let mut backends: std::collections::HashMap<String, Arc<dyn PgBackend>> =
        std::collections::HashMap::new();
    backends.insert("db".to_string(), backend);
    let broker = Arc::new(Broker::new(&grants, backends));

    let req = serde_json::json!({"sql": "select 1", "params": []})
        .to_string()
        .into_bytes();
    let mut handles = Vec::new();
    for _ in 0..CALLERS {
        let broker = broker.clone();
        let req = req.clone();
        handles.push(tokio::spawn(async move {
            broker.dispatch("pg.query", "db", &req).await
        }));
    }

    // Let the admitted call reach the (now blocked) backend and the rest get
    // throttled, then release the gate so the admitted call can finish.
    tokio::time::sleep(Duration::from_millis(250)).await;
    gate.wait().await;

    let mut ok = 0;
    let mut throttled = 0;
    for h in handles {
        let bytes = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("no call may hang")
            .expect("task joins");
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if v["ok"] == true {
            ok += 1;
        } else if v["error"]["code"] == "throttled" {
            throttled += 1;
        } else {
            panic!("unexpected response: {v}");
        }
    }

    assert_eq!(ok, ADMITTED, "exactly {ADMITTED} call admitted");
    assert_eq!(
        throttled,
        CALLERS - ADMITTED,
        "the rest throttled, not queued"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        ADMITTED,
        "never more than max_inflight in flight at the backend"
    );
}
