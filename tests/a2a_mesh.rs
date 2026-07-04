//! A2A client mesh, end to end — riz delegating to riz.
//!
//! Spec: docs/superpowers/specs/2026-07-02-a2a-builtin-agent-design.md (PR 3)
//!
//! Boots TWO real riz binaries, fully offline via the mock provider:
//!   B ("warehouse") — has a real bun function and a built-in agent
//!   A ("front-desk") — has NO functions; its only tool is the peer
//!                       `delegate_to_warehouse` from `[agent.peers]`
//!
//! Delegating a task to A must produce an answer that traveled
//! A → (A2A SendMessage) → B → B's agent loop → B's REAL function → back.
//! Loop protection: a self-peering agent terminates at max_hops instead of
//! delegating forever.

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

const WORKER_FN: &str = r#"
export const lookupOrder = async () => ({
  statusCode: 200,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ order_id: "42", status: "shipped", eta: "2 days" }),
});
"#;

fn worker_toml(port: u16) -> String {
    format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[gateway]
default_provider = "mock"
[gateway.providers.mock]
kind = "mock"

[agent]
name = "warehouse"
description = "Knows order status"
model = "mock"

[function.lookup_order]
runtime = "bun"
handler = "fns.lookupOrder"
timeout_ms = 5000
concurrency = 1
[[function.lookup_order.routes]]
path = "/orders"
method = "GET"
"#
    )
}

fn front_desk_toml(port: u16, warehouse_url: &str) -> String {
    format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[gateway]
default_provider = "mock"
[gateway.providers.mock]
kind = "mock"

[agent]
name = "front-desk"
description = "Delegates order questions to the warehouse"
model = "mock"

[agent.peers]
warehouse = "{warehouse_url}"
"#
    )
}

struct Server {
    child: std::process::Child,
    base: String,
    _dir: tempfile::TempDir,
}

fn boot(toml: String, handler: Option<&str>) -> Server {
    let dir = tempfile::tempdir().expect("tempdir");
    let port: u16 = toml
        .lines()
        .find_map(|l| l.strip_prefix("port = "))
        .unwrap()
        .parse()
        .unwrap();
    std::fs::write(dir.path().join("riz.toml"), toml).unwrap();
    if let Some(h) = handler {
        std::fs::write(dir.path().join("fns.ts"), h).unwrap();
    }
    let child = Command::new(riz_binary())
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");
    assert!(
        wait_for_ready(port, Duration::from_secs(15)),
        "riz not ready"
    );
    Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
        _dir: dir,
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn a2a(base: &str, body: serde_json::Value) -> serde_json::Value {
    reqwest::blocking::Client::new()
        .post(format!("{base}/_riz/a2a"))
        .json(&body)
        .send()
        .expect("a2a request")
        .json()
        .expect("a2a json")
}

fn has_bun() -> bool {
    Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[test]
fn front_desk_delegates_to_warehouse_and_answers() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let b_port = pick_free_port();
    let warehouse = boot(worker_toml(b_port), Some(WORKER_FN));
    let a_port = pick_free_port();
    let front_desk = boot(front_desk_toml(a_port, &warehouse.base), None);

    let body = a2a(
        &front_desk.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{
            "message":{"role":"user","messageId":"m1",
                        "parts":[{"kind":"text","text":"where is order 42?"}]}}}),
    );
    let task = &body["result"];
    assert_eq!(
        task["status"]["state"], "completed",
        "the delegation chain must complete: {body}"
    );
    let text = task["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default();
    // The answer traveled A → B → B's REAL bun function → back to A.
    assert!(
        text.contains("shipped"),
        "warehouse's function output must surface in front-desk's answer: {body}"
    );
}

#[test]
fn self_peering_agent_terminates_at_hop_cap() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    // An agent whose only peer is ITSELF — every task delegates to itself.
    // Without hop protection this recurses forever; with it, the chain is cut
    // with a clear hop-limit error the outer task still answers around.
    let port = pick_free_port();
    let url = format!("http://127.0.0.1:{port}");
    let ouroboros = boot(front_desk_toml(port, &url), None);

    let started = Instant::now();
    let body = a2a(
        &ouroboros.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{
            "message":{"role":"user","messageId":"m1",
                        "parts":[{"kind":"text","text":"echo"}]}}}),
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "hop cap must terminate the chain quickly"
    );
    let task = &body["result"];
    // The outer task completes (the mock answers around the tool error) or
    // fails cleanly — either way it is TERMINAL and mentions the hop limit
    // somewhere in the artifact/status chain.
    let state = task["status"]["state"].as_str().unwrap_or("");
    assert!(
        matches!(state, "completed" | "failed" | "rejected"),
        "terminal state required: {body}"
    );
    assert!(
        body.to_string().contains("hop"),
        "the hop limit must be named: {body}"
    );
}

#[test]
fn a2a_send_cli_prints_the_completed_task() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let b_port = pick_free_port();
    let warehouse = boot(worker_toml(b_port), Some(WORKER_FN));

    let out = Command::new(riz_binary())
        .args(["a2a", "send", &warehouse.base, "where is order 42?"])
        .output()
        .expect("run riz a2a send");
    assert!(
        out.status.success(),
        "cli must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("completed"), "task state shown: {stdout}");
    assert!(
        stdout.contains("shipped"),
        "the answer must be printed: {stdout}"
    );
}
