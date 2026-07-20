//! Secrets canary: a worker's process environment must NOT carry the daemon's
//! secrets. Since PR6 spawns every worker with `env_clear()` + a conservative
//! allowlist, a resource DSN (or any daemon-only var) lives in exactly one
//! process — the daemon. The function's own `[function.X.env]` is the
//! documented escape hatch and DOES reach the worker.
//!
//! Drives the real riz binary + a bun handler that reflects `process.env`.

use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_riz"))
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0")
        .local_addr()
        .expect("addr")
        .port()
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
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

#[test]
fn worker_env_is_scrubbed_of_daemon_secrets() {
    if Command::new("bun").arg("--version").output().is_err() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/env-dump");
    let port = pick_free_port();
    let config = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[function.env-dump]
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
concurrency = 1

# Escape hatch: a function's own env DOES reach its worker.
[function.env-dump.env]
KEPT = "yes"

[[function.env-dump.routes]]
path = "/env"
method = "GET"
"#,
        handler = fixture.join("index.handler").to_string_lossy(),
    );
    let config_path = tmp.path().join("riz.toml");
    std::fs::File::create(&config_path)
        .unwrap()
        .write_all(config.as_bytes())
        .unwrap();

    // Two daemon-only secrets: a DSN-shaped var and a bare canary. Neither is
    // on the allowlist, so neither may reach the worker.
    let mut server = Command::new(riz_binary())
        .args(["--log-level", "warn", "--config"])
        .arg(&config_path)
        .arg("run")
        .env("RIZ_PG_MAIN_DSN", "postgres://secret@db/prod")
        .env("RIZ_SECRET_CANARY", "do-not-leak-me")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz");

    if !wait_for_ready(port, Duration::from_secs(15)) {
        let _ = server.kill();
        panic!("riz never became ready");
    }
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/env")).expect("GET /env");
    let body: serde_json::Value = resp.json().unwrap_or_default();
    let _ = server.kill();
    let _ = server.wait();

    let env = &body["env"];
    let dump = env.to_string();

    // (a) daemon secrets are absent — by key AND by value (belt and braces).
    assert!(
        env.get("RIZ_SECRET_CANARY").is_none(),
        "canary key leaked: {dump}"
    );
    assert!(
        env.get("RIZ_PG_MAIN_DSN").is_none(),
        "DSN key leaked: {dump}"
    );
    assert!(
        !dump.contains("do-not-leak-me"),
        "canary value leaked: {dump}"
    );
    assert!(!dump.contains("secret@db"), "DSN value leaked: {dump}");

    // (b) the allowlist and the escape hatch still work.
    assert!(
        env.get("PATH").is_some(),
        "PATH should pass the allowlist: {dump}"
    );
    assert_eq!(
        env["KEPT"], "yes",
        "[function.env] escape hatch must reach the worker: {dump}"
    );
}
