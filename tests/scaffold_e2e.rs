//! End-to-end scaffold tests: prove that `riz init <template>` produces a
//! project that actually boots and serves traffic. Distinct from `cli_init.rs`
//! which only verifies the files exist + the config parses.
//!
//! Each test spawns the riz binary, polls /ready, hits the documented curl
//! URL, and asserts the handler responded with the shape its README promises.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0")
        .local_addr()
        .unwrap()
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

/// Rewrite the scaffolded riz.toml's [server] port to a free port so tests
/// don't collide with each other or with a developer's running riz.
fn rewrite_port(toml_path: &std::path::Path, port: u16) {
    let s = std::fs::read_to_string(toml_path).expect("read toml");
    let new = s.replace("port = 3000", &format!("port = {port}"));
    std::fs::write(toml_path, new).expect("rewrite toml");
}

#[test]
fn typescript_http_scaffold_boots_and_serves_hello() {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("typescript_http_scaffold_boots_and_serves_hello: bun missing — skipping");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");

    Command::new(riz_binary())
        .args(["init", "typescript-http"])
        .arg(&target)
        .output()
        .expect("scaffold");
    let port = pick_free_port();
    rewrite_port(&target.join("riz.toml"), port);

    let mut server = Command::new(riz_binary())
        .args(["--no-tui", "--log-level", "warn", "run"])
        .current_dir(&target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz");
    let ready = wait_for_ready(port, Duration::from_secs(15));
    if !ready {
        let _ = server.kill();
        panic!("typescript-http scaffold never became ready");
    }

    let resp = reqwest::blocking::get(format!(
        "http://127.0.0.1:{port}/hello?name=alice"
    ))
    .expect("curl");
    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    let _ = server.kill();
    let _ = server.wait();

    assert!(
        status.is_success(),
        "scaffold returned {status}, body: {body}"
    );
    assert!(
        body.contains("hello, alice"),
        "expected 'hello, alice' in body; got: {body}"
    );
    assert!(
        body.contains("functionName") && body.contains("awsRequestId"),
        "expected Lambda context fields in body; got: {body}"
    );
}

/// REGRESSION: `riz init python-http` used to produce a `handler = "main.lambda_handler"`
/// config that crashed the Python adapter with "ModuleNotFoundError: main" because
/// the adapter's bare-module branch tried `importlib.import_module("main")` which
/// can't find files in the user's CWD. Fix: scaffold uses `./main.lambda_handler`
/// which hits the file-path branch. This test locks in the runnable behavior.
#[test]
fn python_http_scaffold_boots_and_serves_hello() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python_http_scaffold_boots_and_serves_hello: python3 missing — skipping");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");

    Command::new(riz_binary())
        .args(["init", "python-http"])
        .arg(&target)
        .output()
        .expect("scaffold");
    let port = pick_free_port();
    rewrite_port(&target.join("riz.toml"), port);

    let mut server = Command::new(riz_binary())
        .args(["--no-tui", "--log-level", "warn", "run"])
        .current_dir(&target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz");
    let ready = wait_for_ready(port, Duration::from_secs(15));
    if !ready {
        let _ = server.kill();
        panic!("python-http scaffold never became ready");
    }

    let resp = reqwest::blocking::get(format!(
        "http://127.0.0.1:{port}/hello?name=alice"
    ))
    .expect("curl");
    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    let _ = server.kill();
    let _ = server.wait();

    assert!(
        status.is_success(),
        "scaffold returned {status}, body: {body} \
         — if this says 'process error: Broken pipe', the python adapter \
         couldn't find main.py (handler= path needs ./ prefix)"
    );
    assert!(
        body.contains("hello, alice"),
        "expected 'hello, alice' in body; got: {body}"
    );
    assert!(
        body.contains("functionName") && body.contains("awsRequestId"),
        "expected Lambda context fields in body; got: {body}"
    );
}
