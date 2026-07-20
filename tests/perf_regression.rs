//! Performance regression harness — scoped to the built `riz` binary.
//!
//! Measures two hot paths and guards them with MACHINE-PORTABLE invariants
//! (so a noisy CI runner never flakes), records absolute numbers for a trend,
//! and offers an OPT-IN baseline band gate for a quiet/designated machine:
//!
//! - HTTP dispatch: throughput + p50/p99 for a warm Bun handler.
//! - Capability path: p50/p99 of a brokered `pg.query` over the daemon UDS
//!   (against the in-process pg wire mock) — guards the PR5 broker hop.
//!
//! Always asserts: a conservative absolute floor + tail-sanity ratios that
//! hold across machines. Writes `target/perf-latest.json` for trend. With
//! `RIZ_PERF_GATE=1` and a committed `tests/perf_baseline.json`, ALSO asserts
//! each metric within a wide band (>= 0.6x baseline). `RIZ_PERF_UPDATE_BASELINE=1`
//! rewrites the baseline from this run.
//!
//! Isolated nextest binary (own CI step) — excluded from the default filter.

#[path = "pg_wire_mock/mod.rs"]
mod pg_wire_mock;

use pg_wire_mock::MockPgServer;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WARMUP: usize = 32;
const HTTP_TOTAL: usize = 800;
const HTTP_CONCURRENCY: usize = 8;
const CAP_TOTAL: usize = 200;
/// The always-on catastrophe floor (matches perf_http_floor's posture).
const HTTP_FLOOR_RPS: f64 = 120.0;
/// Tail-sanity: p99 must not exceed this multiple of p50 (machine-portable).
const TAIL_RATIO_MAX: f64 = 40.0;
/// The opt-in baseline band: a metric below this fraction of baseline fails.
const BASELINE_BAND: f64 = 0.6;

fn riz_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_riz"))
}

fn bun_available() -> bool {
    Command::new("bun").arg("--version").output().is_ok()
}

/// The broker-wasm guest (built for wasm32-wasip1); `None` if not built.
fn guest_wasm() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/broker-wasm/target/wasm32-wasip1/release/broker-wasm.wasm");
    (p.exists() && p.metadata().map(|m| m.len() > 0).unwrap_or(false)).then_some(p)
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_ready(port: u16, deadline: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/ready");
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(r) = reqwest::blocking::get(&url) {
            if r.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

struct RizProc(Child);
impl Drop for RizProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// p50/p99 in milliseconds from a set of durations.
fn percentiles(mut samples: Vec<Duration>) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    samples.sort();
    let at = |q: f64| {
        let idx = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples.get(idx).copied().unwrap_or_default().as_secs_f64() * 1000.0
    };
    (at(0.50), at(0.99))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn perf_regression() {
    if !bun_available() {
        eprintln!("SKIP perf_regression: bun not on PATH");
        return;
    }

    // A pg mock so the capability path measures the real UDS broker hop.
    let mock = MockPgServer::start().await;
    let dsn = mock.dsn();
    let has_wasm = guest_wasm().is_some();

    let port = pick_free_port();
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = write_config(dir.path(), port, has_wasm);
    let mut server = RizProc(
        Command::new(riz_binary())
            .args(["--log-level", "warn", "--config"])
            .arg(&cfg)
            .arg("run")
            .env("RIZ_PG_E2E_DSN", &dsn)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn riz"),
    );
    let ready = tokio::task::spawn_blocking(move || wait_ready(port, Duration::from_secs(30)))
        .await
        .unwrap();
    if !ready {
        let _ = server.0.kill();
        panic!("riz never became ready");
    }

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(HTTP_CONCURRENCY)
        .build()
        .unwrap();
    let http_url = format!("http://127.0.0.1:{port}/http");

    // Warm every worker so the measured window has no cold starts.
    for _ in 0..WARMUP {
        let _ = client.get(&http_url).send().await;
    }

    // ── HTTP throughput + latency ────────────────────────────────────────
    let per_task = HTTP_TOTAL / HTTP_CONCURRENCY;
    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..HTTP_CONCURRENCY {
        let client = client.clone();
        let url = http_url.clone();
        handles.push(tokio::spawn(async move {
            let mut lats = Vec::with_capacity(per_task);
            let mut ok = 0usize;
            for _ in 0..per_task {
                let t = Instant::now();
                if let Ok(r) = client.get(&url).send().await {
                    if r.status() == 200 {
                        ok += 1;
                    }
                }
                lats.push(t.elapsed());
            }
            (ok, lats)
        }));
    }
    let mut http_lats = Vec::new();
    let mut ok = 0usize;
    for h in handles {
        let (n, lats) = h.await.unwrap();
        ok += n;
        http_lats.extend(lats);
    }
    let http_secs = start.elapsed().as_secs_f64();
    let http_rps = ok as f64 / http_secs;
    let (http_p50, http_p99) = percentiles(http_lats);

    // ── Capability (brokered pg.query over the UDS) latency ───────────────
    let (cap_p50, cap_p99) = if has_wasm {
        let cap_url = format!("http://127.0.0.1:{port}/db-orders");
        for _ in 0..8 {
            let _ = client.get(&cap_url).send().await;
        }
        let mut lats = Vec::with_capacity(CAP_TOTAL);
        for _ in 0..CAP_TOTAL {
            let t = Instant::now();
            let _ = client.get(&cap_url).send().await;
            lats.push(t.elapsed());
        }
        percentiles(lats)
    } else {
        eprintln!("perf_regression: broker-wasm guest not built — skipping capability metric");
        (0.0, 0.0)
    };

    drop(server);

    let report = serde_json::json!({
        "http_rps": (http_rps).round(),
        "http_p50_ms": (http_p50 * 100.0).round() / 100.0,
        "http_p99_ms": (http_p99 * 100.0).round() / 100.0,
        "cap_p50_ms": (cap_p50 * 100.0).round() / 100.0,
        "cap_p99_ms": (cap_p99 * 100.0).round() / 100.0,
    });
    eprintln!(
        "── perf trend ──\n{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    let latest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/perf-latest.json");
    let _ = std::fs::write(&latest, serde_json::to_string_pretty(&report).unwrap());

    // ── Machine-portable assertions (always on) ──────────────────────────
    assert!(
        ok >= HTTP_TOTAL - HTTP_TOTAL / 100,
        "perf: only {ok}/{HTTP_TOTAL} HTTP requests succeeded — a broken pool, not a hiccup"
    );
    assert!(
        http_rps >= HTTP_FLOOR_RPS,
        "perf: {http_rps:.0} req/s below the {HTTP_FLOOR_RPS:.0} floor — a large dispatch regression"
    );
    if http_p50 > 0.0 {
        assert!(
            http_p99 <= http_p50 * TAIL_RATIO_MAX,
            "perf: HTTP p99 {http_p99:.1}ms is > {TAIL_RATIO_MAX}x p50 {http_p50:.1}ms — a tail-latency regression"
        );
    }
    if has_wasm && cap_p50 > 0.0 {
        assert!(
            cap_p99 <= cap_p50 * TAIL_RATIO_MAX,
            "perf: capability p99 {cap_p99:.1}ms is > {TAIL_RATIO_MAX}x p50 {cap_p50:.1}ms"
        );
    }

    // ── Opt-in baseline band gate ────────────────────────────────────────
    maybe_gate_or_update_baseline(&report);
}

/// Update or gate against `tests/perf_baseline.json`, per env flags.
fn maybe_gate_or_update_baseline(report: &serde_json::Value) {
    let baseline_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/perf_baseline.json");
    if std::env::var("RIZ_PERF_UPDATE_BASELINE").is_ok() {
        std::fs::write(
            &baseline_path,
            serde_json::to_string_pretty(report).unwrap(),
        )
        .expect("write baseline");
        eprintln!("perf: baseline updated at {}", baseline_path.display());
        return;
    }
    if std::env::var("RIZ_PERF_GATE").is_err() {
        return; // trend-only on unspecified machines (e.g. noisy CI).
    }
    let Ok(raw) = std::fs::read_to_string(&baseline_path) else {
        eprintln!(
            "perf: RIZ_PERF_GATE set but no baseline — run once with RIZ_PERF_UPDATE_BASELINE=1"
        );
        return;
    };
    let baseline: serde_json::Value = serde_json::from_str(&raw).expect("baseline json");
    // Throughput gates on a lower bound (higher is better)…
    let (cur_rps, base_rps) = (num(report, "http_rps"), num(&baseline, "http_rps"));
    assert!(
        base_rps <= 0.0 || cur_rps >= base_rps * BASELINE_BAND,
        "perf gate: http_rps {cur_rps:.0} < {:.0} ({BASELINE_BAND}x baseline {base_rps:.0})",
        base_rps * BASELINE_BAND
    );
    // …latency metrics gate on an upper bound (1/BASELINE_BAND slack).
    for key in ["http_p99_ms", "cap_p99_ms"] {
        let (cur, base) = (num(report, key), num(&baseline, key));
        assert!(
            base <= 0.0 || cur <= base / BASELINE_BAND,
            "perf gate: {key} {cur:.1}ms > {:.1}ms (1/{BASELINE_BAND} x baseline {base:.1}ms)",
            base / BASELINE_BAND
        );
    }
}

fn num(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn write_config(dir: &std::path::Path, port: u16, with_wasm: bool) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chaos-handler");
    let mut toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

# concurrency matches the load driver so the throughput window measures
# dispatch, not the (correct) reject-not-queue shedding the chaos suite tests.
[function.http]
runtime = "bun"
handler = "{handler}/index.handler"
timeout_ms = 5000
concurrency = 8

[[function.http.routes]]
path = "/http"
method = "GET"
"#,
        handler = fixture.display(),
    );
    if with_wasm {
        let wasm = guest_wasm().unwrap();
        toml.push_str(&format!(
            r#"
[resources.pg.main]
dsn_env = "RIZ_PG_E2E_DSN"

[function.db_orders]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 10000
concurrency = 2

[[function.db_orders.routes]]
path = "/db-orders"
method = "GET"

[function.db_orders.capabilities.db]
type = "pg"
resource = "pg.main"
call_timeout_ms = 5000
"#,
            wasm = wasm.display(),
        ));
    }
    let path = dir.join("riz.toml");
    std::fs::write(&path, toml).expect("write config");
    path
}
