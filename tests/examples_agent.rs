//! Phase 4 — the agent-substrate proof.
//!
//! Boots riz with `examples/riz.agent.toml` and asserts, over `/_riz/mcp`
//! (Streamable HTTP, JSON-RPC 2.0), the EXACT path the Claude Agent SDK
//! drives in `examples/agent-sdk/agent_demo.py`:
//!
//!   (a) `tools/list` exposes the agent-tools functions as named MCP tools,
//!       each carrying an input schema;
//!   (b) `tools/call` on `lookup_order` returns the expected structured
//!       result (order 1042 is delayed);
//!   (c) the LLM gateway `mock` provider round-trips a chat completion —
//!       the AI path, deterministic, no API key.
//!
//! Deterministic and keyless: it proves the substrate the SDK demo needs
//! without ever calling a real model. Mirrors the boot-riz harness used by
//! `tests/cli_mcp_inspect.rs`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0 must succeed")
        .local_addr()
        .unwrap()
        .port()
}

/// Write a copy of examples/riz.agent.toml with the port overridden so the
/// suite can run in parallel without colliding on :3000. Handler paths stay
/// relative; we boot riz with the workspace as its working directory so they
/// resolve exactly as a developer's `riz --config examples/riz.agent.toml run`
/// would.
fn write_agent_config(port: u16) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let src = workspace().join("examples/riz.agent.toml");
    let original = std::fs::read_to_string(&src).expect("read examples/riz.agent.toml");
    // The example pins port = 3000; rewrite just that line.
    let patched = original.replace("port = 3000", &format!("port = {port}"));
    assert!(
        patched.contains(&format!("port = {port}")),
        "expected to rewrite the port line in riz.agent.toml"
    );
    std::fs::write(dir.path().join("riz.toml"), patched).expect("write patched config");
    dir
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    let url = format!("http://127.0.0.1:{port}/ready");
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

fn mcp_call(client: &reqwest::blocking::Client, url: &str, body: serde_json::Value) -> serde_json::Value {
    client
        .post(url)
        .json(&body)
        .send()
        .expect("POST /_riz/mcp")
        .json()
        .expect("MCP response is JSON")
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn agent_tools_are_mcp_tools_and_callable_over_riz_mcp() {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("agent_tools substrate test: bun not on PATH — skipping");
        return;
    }

    let port = pick_free_port();
    let cfg_dir = write_agent_config(port);

    let server = Command::new(riz_binary())
        .args([
            "--config",
            cfg_dir.path().join("riz.toml").to_str().unwrap(),
            "run",
        ])
        // Relative handler paths in riz.agent.toml resolve against the repo root.
        .current_dir(workspace())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz run");
    let mut server = Server(server);

    if !wait_for_ready(port, Duration::from_secs(20)) {
        let _ = server.0.kill();
        panic!("riz never became ready within 20s");
    }

    let url = format!("http://127.0.0.1:{port}/_riz/mcp");
    let client = reqwest::blocking::Client::new();

    // (a) tools/list exposes each agent-tools function as a named MCP tool
    //     with an input schema — the discovery step the Agent SDK performs.
    let list = mcp_call(
        &client,
        &url,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let tools = list["result"]["tools"]
        .as_array()
        .expect("tools/list result has a tools array");
    for want in ["lookup_order", "list_inventory", "create_ticket"] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == want)
            .unwrap_or_else(|| panic!("tools/list must include MCP tool {want:?}; got {tools:?}"));
        // Each tool must declare an object input schema so a client can type it.
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "tool {want:?} must carry an object inputSchema; got {tool:?}"
        );
    }

    // (a2) Typed schemas (v1 roadmap #13) surface from the example config:
    //      lookup_order's {id} path param is typed + required straight from
    //      the route template, and create_ticket carries the declared body
    //      schema + description override from [function.create_ticket.mcp].
    let lookup = tools.iter().find(|t| t["name"] == "lookup_order").unwrap();
    let schema = &lookup["inputSchema"];
    assert_eq!(
        schema["properties"]["pathParams"]["properties"]["id"]["type"], "string",
        "lookup_order must type the id path param; got {schema}"
    );
    assert!(
        schema["properties"]["pathParams"]["required"]
            .as_array()
            .map(|a| a.iter().any(|v| v == "id"))
            .unwrap_or(false),
        "id must be required; got {schema}"
    );
    let ticket = tools.iter().find(|t| t["name"] == "create_ticket").unwrap();
    assert!(
        ticket["description"]
            .as_str()
            .unwrap_or("")
            .contains("support ticket"),
        "create_ticket must carry its mcp description override; got {ticket:?}"
    );
    let body_schema = &ticket["inputSchema"]["properties"]["body"];
    assert_eq!(
        body_schema["properties"]["orderId"]["type"], "string",
        "create_ticket body schema must type orderId; got {body_schema}"
    );
    assert!(
        body_schema["required"]
            .as_array()
            .map(|a| a.iter().any(|v| v == "orderId"))
            .unwrap_or(false),
        "orderId must be required in the body schema; got {body_schema}"
    );

    // (b) tools/call on lookup_order returns the expected structured result.
    //     Order 1042 is the canonical delayed order — this is exactly the
    //     call the Agent SDK demo makes first.
    let call = mcp_call(
        &client,
        &url,
        serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"lookup_order","arguments":{"pathParams":{"id":"1042"}}}
        }),
    );
    let structured = &call["result"]["structuredContent"];
    assert_eq!(
        structured["statusCode"], 200,
        "lookup_order on 1042 must succeed; got {call:?}"
    );
    // The Lambda response body is a JSON string; parse it and assert the order.
    let inner_body = structured["body"].as_str().expect("response body string");
    let order: serde_json::Value =
        serde_json::from_str(inner_body).expect("lookup_order body is JSON");
    assert_eq!(order["id"], "1042");
    assert_eq!(
        order["delayed"], true,
        "order 1042 must report delayed:true so the agent opens a ticket; got {order:?}"
    );
    assert_eq!(call["result"]["isError"], false);

    // (b2) Typed body call: the schema invites a JSON OBJECT body — riz must
    //      serialize it into the Lambda event's string body and the real bun
    //      handler must read it. End-to-end proof the typed schema is
    //      actually callable, not just listable.
    let ticket_call = mcp_call(
        &client,
        &url,
        serde_json::json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"create_ticket","arguments":{
                "body": {"orderId": "1042", "reason": "order delayed 9 days"}
            }}
        }),
    );
    let structured = &ticket_call["result"]["structuredContent"];
    assert_eq!(
        structured["statusCode"], 201,
        "create_ticket with an object body must succeed; got {ticket_call:?}"
    );
    let inner_body = structured["body"].as_str().expect("ticket body string");
    let created: serde_json::Value =
        serde_json::from_str(inner_body).expect("create_ticket body is JSON");
    assert_eq!(
        created["ticket"]["orderId"], "1042",
        "handler must have parsed the serialized object body; got {created}"
    );

    // (c) The AI path: the gateway mock provider round-trips a chat
    //     completion — deterministic, no key. This is the model side the
    //     Agent SDK demo uses against a real provider.
    let chat: serde_json::Value = client
        .post(format!("http://127.0.0.1:{port}/_riz/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mock",
            "messages": [{"role": "user", "content": "open a ticket for order 1042"}],
            "stream": false
        }))
        .send()
        .expect("POST /_riz/v1/chat/completions")
        .json()
        .expect("chat completion is JSON");
    assert_eq!(chat["object"], "chat.completion");
    assert_eq!(chat["choices"][0]["message"]["role"], "assistant");
    assert!(
        chat["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0,
        "mock gateway must attribute tokens; got {chat:?}"
    );

    drop(server); // explicit teardown before the test returns
}
