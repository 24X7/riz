//! Per-function environment variables: `[function.X.env]` in riz.toml is
//! injected into the worker process environment at spawn — the standard way
//! to hand a handler its `DATABASE_URL` / API keys without global exports.
//!
//! Spawns the real riz binary against a temp project (node runtime — plain
//! ESM, no build step) and asserts the handler actually sees the values.

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

const HANDLER: &str = r#"
export const handler = async () => ({
  statusCode: 200,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({
    my_api_key: process.env.MY_API_KEY ?? null,
    database_url: process.env.DATABASE_URL ?? null,
  }),
});
"#;

fn project_toml(port: u16) -> String {
    format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[function.hello]
runtime = "node"
handler = "index.handler"
timeout_ms = 5000
concurrency = 1

[function.hello.env]
MY_API_KEY = "sekrit-123"
DATABASE_URL = "postgres://localhost/app"

[[function.hello.routes]]
path = "/hello"
method = "GET"
"#
    )
}

#[test]
fn function_env_reaches_the_worker_process() {
    if Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: node not on PATH");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    std::fs::write(dir.path().join("riz.toml"), project_toml(port)).unwrap();
    std::fs::write(dir.path().join("index.mjs"), HANDLER).unwrap();

    let mut child = Command::new(riz_binary())
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");

    let ready = wait_for_ready(port, Duration::from_secs(15));
    if !ready {
        let _ = child.kill();
        panic!("riz never became ready");
    }

    let body: serde_json::Value = reqwest::blocking::get(format!("http://127.0.0.1:{port}/hello"))
        .expect("GET /hello")
        .json()
        .expect("json body");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        body["my_api_key"], "sekrit-123",
        "[function.X.env] must reach the worker process: {body}"
    );
    assert_eq!(body["database_url"], "postgres://localhost/app", "{body}");
}
