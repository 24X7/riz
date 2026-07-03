//! A2A server core, end to end — riz as a built-in agent.
//!
//! Spec: docs/superpowers/specs/2026-07-02-a2a-builtin-agent-design.md
//!
//! Boots the REAL riz binary with `[agent]` + the mock gateway + two bun HTTP
//! functions and drives it over A2A JSON-RPC — fully offline: the mock
//! provider deterministically calls the first tool, the runtime executes it
//! through the same path as MCP tools/call, and the mock's second turn
//! produces the final answer. Delegate → reason → act → answer, zero keys.

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

const HANDLERS: &str = r#"
export const lookupOrder = async (event: any) => ({
  statusCode: 200,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ order_id: "42", status: "shipped", eta: "2 days" }),
});

export const hidden = async () => ({ statusCode: 200, body: "secret" });
"#;

fn project_toml(port: u16) -> String {
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
name = "shop-support"
description = "Answers order questions using the shop's own functions"
model = "mock"
system_prompt = "You are a concise support agent."
tools = ["lookup_order"]

[function.lookup_order]
runtime = "bun"
handler = "fns.lookupOrder"
timeout_ms = 5000
concurrency = 1

[[function.lookup_order.routes]]
path = "/orders"
method = "GET"

[function.hidden]
runtime = "bun"
handler = "fns.hidden"
timeout_ms = 5000
concurrency = 1

[[function.hidden.routes]]
path = "/hidden"
method = "GET"
"#
    )
}

struct Server {
    child: std::process::Child,
    base: String,
    _dir: tempfile::TempDir,
}

fn boot() -> Server {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    std::fs::write(dir.path().join("riz.toml"), project_toml(port)).unwrap();
    std::fs::write(dir.path().join("fns.ts"), HANDLERS).unwrap();
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
fn agent_card_is_served_with_allowlisted_skills() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    let card: serde_json::Value =
        reqwest::blocking::get(format!("{}/.well-known/agent-card.json", srv.base))
            .expect("card")
            .json()
            .expect("card json");
    assert_eq!(card["name"], "shop-support", "{card}");
    assert_eq!(card["preferredTransport"], "JSONRPC", "{card}");
    assert!(
        card["url"].as_str().unwrap().ends_with("/_riz/a2a"),
        "{card}"
    );
    assert_eq!(card["capabilities"]["streaming"], false, "{card}");
    let skills: Vec<&str> = card["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(skills.contains(&"lookup_order"), "{card}");
    assert!(
        !skills.contains(&"hidden"),
        "allowlist must gate skills: {card}"
    );
}

#[test]
fn send_message_runs_the_agent_loop_to_completion() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    let body = a2a(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{
            "message":{"role":"user","messageId":"m1",
                        "parts":[{"kind":"text","text":"where is order 42?"}]}}}),
    );
    let task = &body["result"];
    assert_eq!(
        task["status"]["state"], "completed",
        "task must complete offline via mock: {body}"
    );
    let task_id = task["id"].as_str().expect("task id");

    // The artifact is the agent's final answer, which the mock provider
    // builds FROM the executed tool result — proving the loop really ran:
    // delegate → model tool_call → lookup_order executed → result → answer.
    let text = task["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("tool result received"),
        "answer must come from the post-tool turn: {body}"
    );
    assert!(
        text.contains("shipped"),
        "the REAL function's output must be inside the answer: {body}"
    );

    // GetTask returns the same task.
    let body = a2a(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id": task_id}}),
    );
    assert_eq!(body["result"]["id"], task_id, "{body}");
    assert_eq!(body["result"]["status"]["state"], "completed", "{body}");
}

#[test]
fn unknown_task_and_cancel_errors_are_clean() {
    if !has_bun() {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    let body = a2a(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"GetTask","params":{"id":"nope"}}),
    );
    assert_eq!(body["error"]["code"], -32001, "TaskNotFound: {body}");

    let body = a2a(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"CancelTask","params":{"id":"nope"}}),
    );
    assert_eq!(body["error"]["code"], -32001, "TaskNotFound: {body}");
}

#[test]
fn agent_without_gateway_fails_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("riz.toml"),
        r#"
[server]
port = 0
host = "127.0.0.1"

[agent]
name = "no-brain"
model = "mock"
"#,
    )
    .unwrap();
    let out = Command::new(riz_binary())
        .current_dir(dir.path())
        .arg("validate")
        .output()
        .expect("run validate");
    assert!(
        !out.status.success(),
        "[agent] without [gateway] must fail validation"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        format!("{stderr}{stdout}").contains("[agent] requires [gateway]"),
        "clear error expected; got stderr={stderr} stdout={stdout}"
    );
}
