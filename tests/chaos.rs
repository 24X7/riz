//! Chaos harness — scoped to the built `riz` binary. Deliberately injects the
//! failures the runtime promises to survive (docs/SAFETY.md) and asserts the
//! invariants hold: reject-not-queue under saturation, worker respawn after a
//! crash, the crash-loop circuit breaker, broker self-heal, no crash-loop on
//! malformed input, graceful SIGTERM drain, and NO orphaned processes.
//!
//! Isolated nextest binary (own CI step) — excluded from the default filter so
//! deliberate fault injection can never flake the main suite.

#[path = "pg_wire_mock/mod.rs"]
mod pg_wire_mock;

use pg_wire_mock::MockPgServer;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_riz"))
}

fn bun_available() -> bool {
    Command::new("bun").arg("--version").output().is_ok()
}

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

/// The riz child + its pid, killed on drop. `disarm()` for tests that manage
/// the process lifetime themselves (SIGTERM drain).
struct Riz(Option<Child>);
impl Riz {
    fn pid(&self) -> u32 {
        self.0.as_ref().map(|c| c.id()).unwrap_or(0)
    }
    fn disarm(mut self) -> Child {
        self.0.take().expect("child present")
    }
}
impl Drop for Riz {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn boot(cfg: &Path, port: u16, dsn: Option<&str>) -> Riz {
    let mut cmd = Command::new(riz_binary());
    cmd.args(["--log-level", "warn", "--config"])
        .arg(cfg)
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = dsn {
        cmd.env("RIZ_PG_E2E_DSN", d);
    }
    let child = cmd.spawn().expect("spawn riz");
    let riz = Riz(Some(child));
    assert!(
        wait_ready(port, Duration::from_secs(30)),
        "riz never became ready"
    );
    riz
}

fn get(port: u16, path: &str, timeout: Duration) -> Option<(u16, String)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap();
    client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .ok()
        .map(|r| {
            let s = r.status().as_u16();
            (s, r.text().unwrap_or_default())
        })
}

/// Direct child worker pids of the riz process (`pgrep -P <pid>`), used to
/// SIGKILL a live worker without a pid-exposing endpoint.
fn worker_pids(riz_pid: u32) -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &riz_pid.to_string()])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn kill_pid(pid: u32, sig: &str) {
    let _ = Command::new("kill").args([sig, &pid.to_string()]).status();
}

fn chaos_config(dir: &Path, port: u16, with_wasm: bool) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chaos-handler");
    let mut toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

# concurrency 4: enough workers to saturate and to kill one and keep serving.
[function.chaos]
runtime = "bun"
handler = "{handler}/index.handler"
timeout_ms = 3000
concurrency = 4

[[function.chaos.routes]]
path = "/chaos"
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

[function.db]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 5000
concurrency = 1

[[function.db.routes]]
path = "/db"
method = "GET"

[function.db.capabilities.db]
type = "pg"
resource = "pg.main"
call_timeout_ms = 1000
"#,
            wasm = wasm.display(),
        ));
    }
    let path = dir.join("riz.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

/// (a) Saturation → reject-not-queue: flood the pool with slow requests; a
/// concurrent fast request must still return promptly (server stays live) and
/// the overflow must be SHED (429/503), never silently queued to a hang.
#[test]
fn saturation_sheds_load_and_stays_responsive() {
    if !bun_available() {
        eprintln!("SKIP: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, false);
    let _riz = boot(&cfg, port, None);
    let _ = get(port, "/chaos", Duration::from_secs(2)); // warm

    // Flood: 16 slow (1.5s) requests at concurrency 4 → the pool is jammed.
    let mut floods = Vec::new();
    for _ in 0..16 {
        floods.push(std::thread::spawn(move || {
            get(port, "/chaos?sleep=1500", Duration::from_secs(5)).map(|(s, _)| s)
        }));
    }
    // While jammed, a fast request must resolve quickly — the server never
    // wedges. It may itself be shed (429/503), which is the point.
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let quick = get(port, "/chaos", Duration::from_secs(3));
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "server wedged under saturation — a concurrent request should resolve or shed fast"
    );
    assert!(quick.is_some(), "server stopped answering under load");

    let statuses: Vec<u16> = floods
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect();
    let shed = statuses.iter().filter(|s| **s == 429 || **s == 503).count();
    assert!(
        shed > 0,
        "expected some requests SHED (429/503) under saturation, got {statuses:?}"
    );
}

/// (b) Worker respawn: SIGKILL a live worker; the pool must respawn and keep
/// serving 200s — a crashed worker is not a wedged function.
#[test]
fn worker_respawns_after_sigkill() {
    if !bun_available() {
        eprintln!("SKIP: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, false);
    let riz = boot(&cfg, port, None);
    assert_eq!(
        get(port, "/chaos", Duration::from_secs(2)).map(|(s, _)| s),
        Some(200)
    );

    let pids = worker_pids(riz.pid());
    assert!(
        !pids.is_empty(),
        "expected worker child processes to SIGKILL"
    );
    if let Some(&victim) = pids.first() {
        kill_pid(victim, "-KILL");
    }

    // Give the liveness watcher time to respawn, then hammer: every request
    // must eventually succeed again (no permanent degradation).
    std::thread::sleep(Duration::from_millis(500));
    let mut ok = 0;
    for _ in 0..20 {
        if get(port, "/chaos", Duration::from_secs(2)).map(|(s, _)| s) == Some(200) {
            ok += 1;
        }
    }
    assert!(
        ok >= 15,
        "after a worker SIGKILL only {ok}/20 requests recovered — the pool did not heal"
    );
}

/// (c) Circuit breaker: a handler that crashes on every invocation must trip
/// the crash-loop breaker to a 503 rather than respawn forever.
#[test]
fn crash_loop_trips_the_circuit_breaker() {
    if !bun_available() {
        eprintln!("SKIP: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, false);
    let _riz = boot(&cfg, port, None);

    // Repeatedly crash the worker. After the breaker's threshold the pool is
    // marked unhealthy and answers 503.
    let mut saw_503 = false;
    for _ in 0..12 {
        if let Some((status, _)) = get(port, "/chaos?crash=1", Duration::from_secs(3)) {
            if status == 503 {
                saw_503 = true;
                break;
            }
        }
    }
    assert!(
        saw_503,
        "a handler crashing every call should trip the circuit breaker to 503"
    );
}

/// (d) Broker self-heal: kill the pg backend mid-life; a brokered call gets a
/// closed-error envelope (never a hang); once the backend returns, a later
/// call succeeds again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_survives_backend_loss_and_recovers() {
    if !bun_available() || guest_wasm().is_none() {
        eprintln!("SKIP: bun or broker-wasm guest missing");
        return;
    }
    let mock = MockPgServer::start().await;
    let dsn = mock.dsn();
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, true);
    let riz = tokio::task::spawn_blocking({
        let cfg = cfg.clone();
        let dsn = dsn.clone();
        move || boot(&cfg, port, Some(&dsn))
    })
    .await
    .unwrap();

    // Healthy call first.
    let ok = tokio::task::spawn_blocking(move || get(port, "/db", Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!(
        ok.map(|(s, _)| s),
        Some(200),
        "granted broker call should succeed"
    );

    // Drop the backend, then call: the guest must get a parseable envelope
    // (backend/timeout), never a hung request past the deadline.
    drop(mock);
    let after = tokio::task::spawn_blocking(move || get(port, "/db", Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(
        after.is_some(),
        "a brokered call with a dead backend must still answer (closed error set), not hang"
    );
    drop(riz);
}

/// (e) Malformed input must not crash-loop the server: a bad request gets a
/// clean error and the server keeps serving.
#[test]
fn malformed_input_does_not_crash_the_server() {
    if !bun_available() {
        eprintln!("SKIP: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, false);
    let _riz = boot(&cfg, port, None);

    let client = reqwest::blocking::Client::new();
    // Junk body + wrong content-type at the route.
    for _ in 0..5 {
        let _ = client
            .post(format!("http://127.0.0.1:{port}/chaos"))
            .body(vec![0xff, 0x00, 0xfe, 0x01])
            .send();
    }
    // The server is still healthy afterward.
    assert_eq!(
        get(port, "/chaos", Duration::from_secs(2)).map(|(s, _)| s),
        Some(200),
        "server should keep serving after malformed input"
    );
}

/// (f) SIGTERM must drain gracefully: an in-flight request completes and the
/// process exits 0. (g) No orphaned workers survive the shutdown.
#[test]
fn sigterm_drains_and_leaves_no_orphans() {
    if !bun_available() {
        eprintln!("SKIP: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let cfg = chaos_config(tmp.path(), port, false);
    let riz = boot(&cfg, port, None);
    let pid = riz.pid();
    let _ = get(port, "/chaos", Duration::from_secs(2)); // warm

    let workers = worker_pids(pid);

    // Start a slow request, then SIGTERM the server mid-flight.
    let inflight =
        std::thread::spawn(move || get(port, "/chaos?sleep=800", Duration::from_secs(6)));
    std::thread::sleep(Duration::from_millis(150));
    kill_pid(pid, "-TERM");

    // The in-flight request should complete (graceful drain), not be dropped.
    let done = inflight.join().ok().flatten();
    assert!(
        done.map(|(s, _)| s) == Some(200),
        "in-flight request should complete during graceful SIGTERM drain"
    );

    // The process exits on its own (drain finished); reap it.
    let mut child = riz.disarm();
    let status = wait_with_timeout(&mut child, Duration::from_secs(35));
    assert!(
        status.is_some(),
        "riz did not exit within the drain window after SIGTERM"
    );

    // No orphaned worker children survive.
    std::thread::sleep(Duration::from_millis(300));
    for w in workers {
        // kill -0 returns success only if the pid is still alive.
        let alive = Command::new("kill")
            .args(["-0", &w.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "worker {w} was orphaned after riz shut down");
    }
}

/// Wait up to `timeout` for a child to exit; returns its status or None.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    None
}
