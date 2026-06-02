//! `riz mcp inspect` — end-to-end self-validation tool.
//!
//! Spins up the riz binary, then runs `riz mcp inspect` against it and
//! verifies the human-readable report contains the spec version, the
//! tools list with output-schema annotation, and the success line.

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
    // Bind 0, read the assigned port, close. Race-able but acceptable for
    // tests — collision rate near zero on a developer box.
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0 must succeed")
        .local_addr()
        .unwrap()
        .port()
}

fn write_test_config(port: u16) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cfg_path = dir.path().join("riz.toml");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let handler_path = workspace
        .join("examples/lambdas/ping/index.handler")
        .display()
        .to_string();
    let toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[function.ping]
runtime = "bun"
handler = "{handler_path}"
timeout_ms = 1000
concurrency = 1
"#
    );
    std::fs::write(&cfg_path, toml).expect("write riz.toml");
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

#[test]
fn mcp_inspect_against_nonexistent_endpoint_fails_clearly() {
    // Pick a port and DON'T bind anything to it.
    let port = pick_free_port();
    let url = format!("http://127.0.0.1:{port}/_riz/mcp");

    let out = Command::new(riz_binary())
        .args(["mcp", "inspect", "--url", &url])
        .output()
        .expect("spawn riz mcp inspect");

    assert!(
        !out.status.success(),
        "inspect against dead endpoint must fail (exit non-zero)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to POST") || stderr.contains("error"),
        "stderr must explain the failure; got: {stderr}"
    );
}

#[test]
fn mcp_inspect_against_running_riz_lists_tools_and_spec_version() {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("mcp_inspect_against_running_riz: bun not on PATH — skipping");
        return;
    }
    let port = pick_free_port();
    let cfg_dir = write_test_config(port);

    let mut server = Command::new(riz_binary())
        .args([
            "--no-tui",
            "--config",
            cfg_dir.path().join("riz.toml").to_str().unwrap(),
            "run",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz run");

    let ready = wait_for_ready(port, Duration::from_secs(15));
    if !ready {
        let _ = server.kill();
        panic!("riz never became ready within 15s");
    }

    let url = format!("http://127.0.0.1:{port}/_riz/mcp");
    let out = Command::new(riz_binary())
        .args(["mcp", "inspect", "--url", &url])
        .output()
        .expect("spawn riz mcp inspect");

    // Tear down the server before any assertion can fail mid-test.
    let _ = server.kill();
    let _ = server.wait();

    assert!(
        out.status.success(),
        "inspect must succeed against a healthy server. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Spec version: 2025-11-25 is the marquee claim — surface in the report.
    assert!(
        stdout.contains("2025-11-25"),
        "report must print the spec version 2025-11-25; got: {stdout}"
    );
    // Tools section + the ping function from our test config.
    assert!(
        stdout.contains("Registered tools") && stdout.contains("ping"),
        "report must enumerate the registered tools; got: {stdout}"
    );
    // outputSchema annotation calls out the 2025-06-18+ feature.
    assert!(
        stdout.contains("MCP 2025-06-18+ structured output"),
        "report must annotate outputSchema as 2025-06-18+; got: {stdout}"
    );
    // Final success line guides the user to the next step.
    assert!(
        stdout.contains("MCP endpoint healthy") && stdout.contains("Point Claude"),
        "report must close with a healthy-endpoint cue + next-step hint; got: {stdout}"
    );
}
