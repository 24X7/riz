//! WebSocket functions as MCP tools — ephemeral sessions, end to end.
//!
//! Spec: docs/superpowers/specs/2026-07-02-ws-ephemeral-tool-sessions-design.md
//!
//! Boots the REAL riz binary with three bun WebSocket functions and drives
//! them through POST /_riz/mcp:
//!   chat  — echoes via the @connections push API (the riz WS reply contract)
//!   quiet — never pushes (a silent handler is valid: empty frames, no error)
//!   deny  — rejects $connect (session must surface a JSON-RPC error)

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

const WS_HANDLERS: &str = r#"
const BASE = process.env.RIZ_TEST_BASE_URL || "http://localhost:3000";

export const echo = async (event: any) => {
  const route = event?.requestContext?.routeKey;
  const id = event?.requestContext?.connectionId;
  if (route === "$default") {
    await fetch(`${BASE}/_riz/connections/${id}`, {
      method: "POST",
      body: `echo: ${event.body ?? ""}`,
    });
  }
  return { statusCode: 200 };
};

export const silent = async () => ({ statusCode: 200 });

export const deny = async (event: any) =>
  event?.requestContext?.routeKey === "$connect"
    ? { statusCode: 403 }
    : { statusCode: 200 };
"#;

fn project_toml(port: u16) -> String {
    let env_block =
        format!("[function.{{}}.env]\nRIZ_TEST_BASE_URL = \"http://127.0.0.1:{port}\"\n");
    let mut toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"
"#
    );
    for (name, export, route) in [
        ("chat", "echo", "/ws-chat"),
        ("quiet", "silent", "/ws-quiet"),
        ("deny", "deny", "/ws-deny"),
    ] {
        toml.push_str(&format!(
            r#"
[function.{name}]
runtime = "bun"
protocol = "websocket"
handler = "ws.{export}"
timeout_ms = 5000
concurrency = 1

{env}
[[function.{name}.routes]]
path = "{route}"
method = "GET"
"#,
            env = env_block.replace("{}", name),
        ));
    }
    toml
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
    std::fs::write(dir.path().join("ws.ts"), WS_HANDLERS).unwrap();
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

fn mcp(base: &str, body: serde_json::Value) -> serde_json::Value {
    reqwest::blocking::Client::new()
        .post(format!("{base}/_riz/mcp"))
        .json(&body)
        .send()
        .expect("mcp request")
        .json()
        .expect("mcp json")
}

#[test]
fn ws_functions_are_tools_with_session_semantics() {
    if Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    // 1. tools/list advertises WS functions with the session input schema.
    let body = mcp(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let tools = body["result"]["tools"].as_array().expect("tools");
    let chat = tools
        .iter()
        .find(|t| t["name"] == "chat")
        .unwrap_or_else(|| panic!("WS function must be advertised as a tool: {body}"));
    assert_eq!(
        chat["inputSchema"]["properties"]["message"]["type"], "string",
        "session tools take a message: {chat}"
    );
    assert!(
        chat["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "message"),
        "message is required: {chat}"
    );
    assert!(
        chat["description"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("websocket"),
        "description must name the session semantics: {chat}"
    );

    // 2. tools/call opens an ephemeral session: $connect → $default(message) →
    //    the handler's @connections push comes back as the tool result.
    let body = mcp(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"chat","arguments":{"message":"hello ws"}}}),
    );
    let frames = body["result"]["structuredContent"]["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("session result carries frames: {body}"));
    assert_eq!(frames.len(), 1, "one echo frame: {body}");
    assert_eq!(frames[0], "echo: hello ws", "{body}");

    // 3. Sessions are ephemeral: nothing lingers in the connection registry.
    let conns: serde_json::Value = reqwest::blocking::get(format!("{}/_riz/connections", srv.base))
        .expect("connections")
        .json()
        .expect("json");
    let remaining = conns["connections"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        remaining, 0,
        "ephemeral session must be cleaned up: {conns}"
    );
}

#[test]
fn silent_ws_function_returns_empty_frames_not_an_error() {
    if Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    let started = Instant::now();
    let body = mcp(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"quiet","arguments":{"message":"anyone home?","timeout_ms":700}}}),
    );
    assert!(
        body["error"].is_null(),
        "a silent handler is valid, not an error: {body}"
    );
    let frames = body["result"]["structuredContent"]["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("frames present even when empty: {body}"));
    assert!(frames.is_empty(), "no pushes → no frames: {body}");
    assert!(
        started.elapsed() >= Duration::from_millis(650),
        "must wait out timeout_ms before giving up on a silent handler"
    );
}

#[test]
fn connect_rejection_surfaces_as_jsonrpc_error() {
    if Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: bun not on PATH");
        return;
    }
    let srv = boot();

    let body = mcp(
        &srv.base,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"deny","arguments":{"message":"let me in"}}}),
    );
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("403") || msg.to_lowercase().contains("reject"),
        "a $connect rejection must surface clearly: {body}"
    );
}
