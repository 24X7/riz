//! End-to-end black-box smoke: boot the REAL `riz` binary against
//! examples/riz.all.toml and assert that every example handler — across Bun,
//! Node, Python, Rust, Go, and WASM — works together over real HTTP, WebSocket,
//! the MCP surface, the LLM gateway, and the health/metrics control plane.
//!
//! This is the nextest entry point for `examples/smoke-all.sh`: it puts the full
//! e2e on the `cargo nextest run` / CI path so incoming code is gated against
//! regressions in any example or runtime, alongside the unit tests.
//!
//! The assertions live in the shell harness (status + body for ~30 checks); this
//! wrapper just locates the binary cargo built (CARGO_BIN_EXE_riz), runs the
//! harness on a free port, and fails the test if the harness exits non-zero.
//!
//! Skips cleanly when the full toolchain is absent — the harness needs bun,
//! node, python3, go, and the wasm32-wasip1 rust target to exercise every
//! runtime, so a partial box can't prove "all examples work together." CI
//! installs all of them, so there it runs for real (no pass-by-skip).
//!
//! Run: `cargo nextest run --test e2e_smoke_all`

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tool_available(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").output().is_ok()
}

/// The wasm leg (echo-wasm + orders-wasm) needs the `wasm32-wasip1` target.
/// Confirmed via rustup; if rustup is absent we can't build the guests, so the
/// harness can't boot the full config — treat as unavailable and skip.
fn wasm_target_installed() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "wasm32-wasip1")
        })
        .unwrap_or(false)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn all_examples_work_together() {
    for tool in ["bun", "node", "python3", "go"] {
        if !tool_available(tool) {
            eprintln!("SKIP e2e_smoke_all: '{tool}' not on PATH — full-runtime e2e needs it");
            return;
        }
    }
    if !wasm_target_installed() {
        eprintln!("SKIP e2e_smoke_all: rust target wasm32-wasip1 not installed");
        return;
    }

    let script = manifest_dir().join("examples/smoke-all.sh");
    assert!(script.is_file(), "missing harness at {}", script.display());

    let port = free_port();
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(manifest_dir())
        // Use the binary cargo just built for this test run, not target/release.
        .env("RIZ_BIN", env!("CARGO_BIN_EXE_riz"))
        .env("PORT", port.to_string())
        .output()
        .expect("failed to run examples/smoke-all.sh");

    // Surface the harness's ✓/✗ report in the test log either way.
    println!("{}", String::from_utf8_lossy(&output.stdout));
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        panic!(
            "e2e smoke harness failed (exit {:?}) — see the report above",
            output.status.code()
        );
    }
}
