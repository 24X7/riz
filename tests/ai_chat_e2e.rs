//! End-to-end test for the ai-chat flagship example: boots the REAL riz binary
//! against examples/ai-chat (mock gateway — offline, deterministic) and proves
//! the whole composition works:
//!
//!   React client served by [static]  ─┐
//!   POST /api/chat (Bun handler)      ├─ one binary, one origin
//!   server-side agent loop            │
//!   gateway tool-calling (mock)      ─┘
//!
//! The mock provider deterministically calls the FIRST declared tool
//! (`lookup_order`) on turn 1 and, once a `role:"tool"` result is in context,
//! answers with text incorporating it — so this exercises the exact wire
//! shapes a real provider produces, with zero network.

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

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/ai-chat")
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

/// Copy the example into a temp dir with the port (server + GATEWAY_URL env)
/// rewritten to a free one, so the test never collides with a dev instance.
fn staged_project(port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = example_dir();

    std::fs::create_dir_all(dir.path().join("api")).unwrap();
    std::fs::create_dir_all(dir.path().join("client/dist")).unwrap();

    let toml = std::fs::read_to_string(src.join("riz.toml"))
        .unwrap()
        .replace("port = 3000", &format!("port = {port}"))
        .replace("127.0.0.1:3000", &format!("127.0.0.1:{port}"));
    std::fs::write(dir.path().join("riz.toml"), toml).unwrap();

    std::fs::copy(src.join("api/chat.ts"), dir.path().join("api/chat.ts")).unwrap();
    // The [static] block requires the dir; one real file proves the wiring.
    std::fs::copy(
        src.join("client/dist/index.html"),
        dir.path().join("client/dist/index.html"),
    )
    .unwrap();
    dir
}

#[test]
fn ai_chat_agent_loop_works_end_to_end() {
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

    let port = pick_free_port();
    let project = staged_project(port);

    let mut child = Command::new(riz_binary())
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");

    if !wait_for_ready(port, Duration::from_secs(15)) {
        let _ = child.kill();
        panic!("riz never became ready");
    }
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::new();

    // 1. The UI is served on the same origin.
    let index = client.get(&base).send().expect("GET /");
    assert!(index.status().is_success(), "static client must be served");

    // 2. The chat API runs the full agent loop against the mock gateway.
    let resp = client
        .post(format!("{base}/api/chat"))
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": "where is order 42?"}]
        }))
        .send()
        .expect("POST /api/chat");
    let status = resp.status();
    let body: serde_json::Value = resp.json().expect("json body");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(status, 200, "chat API must succeed: {body}");

    // The handler must have executed a real tool round-trip…
    let trace = body["tool_trace"].as_array().expect("tool_trace array");
    assert!(
        !trace.is_empty(),
        "the agent loop must execute at least one tool: {body}"
    );
    assert_eq!(
        trace[0]["name"], "lookup_order",
        "mock calls the first declared tool: {body}"
    );

    // …and the final reply must incorporate the tool's result (the mock's
    // turn-2 signature echoes the tool result content back).
    let reply = body["reply"].as_str().expect("reply string");
    assert!(
        reply.contains("tool result received"),
        "final answer must come from the post-tool turn: {reply}"
    );

    // 3. Tokens were metered through the gateway.
    assert!(
        body["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0,
        "usage must accumulate across hops: {body}"
    );
}
