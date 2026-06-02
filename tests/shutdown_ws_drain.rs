//! Regression: riz used to hang for the full SHUTDOWN_DRAIN_TIMEOUT (30s)
//! when WebSocket connections were open at shutdown time, because axum's
//! graceful drain waits for in-flight requests and WS handlers don't
//! complete on their own.
//!
//! Fix: server.rs sends OutboundMessage::Close to every WS connection
//! inside the shutdown_signal future, BEFORE axum starts draining. This
//! test boots riz with the bundled `chat` WS function, opens a real WS
//! connection, sends SIGTERM, and asserts the process exits in seconds.
//!
//! Without the fix this test would take ~30s and (with the assertion
//! below) fail. With the fix it passes in ~2s.

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

async fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/ready");
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(r) = reqwest::get(&url).await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

/// Send SIGTERM to the given pid. Uses `/bin/kill -TERM <pid>` instead of
/// pulling in `nix` as a dev-dep — keeps the test minimal.
fn send_sigterm(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_with_open_ws_connection_completes_quickly() {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("shutdown_with_open_ws_connection: bun missing — skipping");
        return;
    }

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = tmp.path().join("riz.toml");
    let port = pick_free_port();
    let chat_handler = workspace
        .join("examples/lambdas/chat/index.handler")
        .display()
        .to_string();
    std::fs::write(
        &cfg,
        format!(
            r#"
[server]
port = {port}
host = "127.0.0.1"

[function.chat]
protocol = "websocket"
runtime = "bun"
handler = "{chat_handler}"
timeout_ms = 5000
concurrency = 4

[[function.chat.routes]]
path = "/chat"
method = "ANY"
"#
        ),
    )
    .expect("write toml");

    let mut server = Command::new(riz_binary())
        .args([
            "--no-tui",
            "--log-level",
            "warn",
            "--config",
            cfg.to_str().unwrap(),
            "run",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz");
    let pid = server.id();

    if !wait_for_ready(port, Duration::from_secs(15)).await {
        let _ = server.kill();
        panic!("riz never became ready");
    }

    // Open a real WebSocket connection — this is the load the drain has
    // to deal with. Use tokio-tungstenite (already a dev-dep).
    let ws_url = format!("ws://127.0.0.1:{port}/chat");
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");
    // Hold the connection by keeping ws_stream alive. We don't need to
    // send any messages — just having an open WS is enough to make
    // axum's drain wait if the close-at-signal fix is missing.

    // Trigger graceful shutdown.
    let shutdown_started = Instant::now();
    send_sigterm(pid);

    // Wait for the process to exit and measure how long it took.
    let mut elapsed = Duration::ZERO;
    for _ in 0..150 {
        // 15s max
        match server.try_wait() {
            Ok(Some(_status)) => {
                elapsed = shutdown_started.elapsed();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    if server.try_wait().ok().flatten().is_none() {
        let _ = server.kill();
        panic!(
            "riz did not exit within 15s after SIGTERM (would have been the old 30s-hang bug)"
        );
    }

    // Keep ws_stream alive until after exit so the test really did hold
    // an open WS through the shutdown window.
    drop(ws_stream);

    assert!(
        elapsed < Duration::from_secs(5),
        "riz took {elapsed:?} to drain with an open WS connection. \
         Pre-fix this would take ~30s (the SHUTDOWN_DRAIN_TIMEOUT). \
         If you see this fail, the close-at-signal path in server.rs \
         regressed: verify shutdown_state.ws_connections is iterated and \
         sent Close BEFORE axum starts the drain."
    );
}
