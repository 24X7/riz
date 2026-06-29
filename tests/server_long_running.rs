//! Regression: server::run used to wrap the entire serve_future in
//! tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, ...), which fires after
//! 30s REGARDLESS of whether a shutdown signal arrived. Result: every
//! `riz run` auto-crashed exactly 30s after boot in real prod use.
//!
//! Fix: the drain timeout only arms after the shutdown_signal future
//! fires (signal_observed oneshot).
//!
//! This test boots riz, waits past the old crash window, asserts it's
//! still serving, then sends SIGTERM and asserts a clean fast exit.
//!
//! ~35s wall-clock cost. The bug it catches is severe enough (riz simply
//! does not work for >30s without it) to justify the runtime.

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

fn send_sigterm(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

#[tokio::test(flavor = "current_thread")]
async fn riz_stays_alive_past_30s_drain_timeout_window() {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("riz_stays_alive_past_30s: bun missing — skipping");
        return;
    }

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = tmp.path().join("riz.toml");
    let port = pick_free_port();
    let ping_handler = workspace
        .join("examples/lambdas/ping/index.handler")
        .display()
        .to_string();
    std::fs::write(
        &cfg,
        format!(
            r#"
[server]
port = {port}
host = "127.0.0.1"

[function.ping]
runtime = "bun"
handler = "{ping_handler}"
timeout_ms = 1000
concurrency = 1
"#
        ),
    )
    .expect("write toml");

    let mut server = Command::new(riz_binary())
        .args([
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

    // SHUTDOWN_DRAIN_TIMEOUT in src/server.rs is 30 seconds. Pre-fix, the
    // unconditional outer timeout wrapping serve_future fired at exactly
    // 30s after boot, regardless of whether a signal arrived. Wait 32s to
    // be safely past the trigger.
    tokio::time::sleep(Duration::from_secs(32)).await;

    // riz must still be serving traffic at this point.
    let still_alive = server.try_wait().expect("try_wait").is_none();
    if !still_alive {
        let _ = server.kill();
        panic!(
            "riz exited within 32s of boot — the pre-fix timeout-from-boot \
             bug is back. Check that src/server.rs only arms the drain \
             timeout AFTER signal_observed_rx resolves."
        );
    }

    // Confirm it's still actually serving (not just deadlocked).
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/ping"))
        .await
        .expect("curl /ping at 32s");
    assert!(
        resp.status().is_success(),
        "expected 200 from /ping at 32s mark, got {}",
        resp.status()
    );

    // Now exercise the graceful-shutdown path. With my fix the drain
    // completes in milliseconds because no requests are in flight.
    let shutdown_start = Instant::now();
    send_sigterm(pid);
    for _ in 0..100 {
        if server.try_wait().ok().flatten().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let exit_elapsed = shutdown_start.elapsed();

    if server.try_wait().ok().flatten().is_none() {
        let _ = server.kill();
        panic!("riz did not exit within 10s of SIGTERM");
    }
    assert!(
        exit_elapsed < Duration::from_secs(5),
        "riz took {exit_elapsed:?} to exit on SIGTERM (expected sub-second)"
    );
}
