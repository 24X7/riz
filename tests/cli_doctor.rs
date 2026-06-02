//! `riz doctor` — pre-flight diagnostic CLI.
//!
//! Spawns the riz binary as a subprocess and verifies each check produces
//! the right severity for the scenario:
//!   - missing riz.toml: rc=1, "not found"
//!   - valid scaffold: rc=0, all ✓
//!   - missing handler file: rc=1, "file not found"
//!   - port already bound: rc=1 (unless the binder is also riz)

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn doctor_fails_when_riz_toml_missing() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = Command::new(riz_binary())
        .args(["doctor"])
        .current_dir(tmp.path())
        .output()
        .expect("spawn riz doctor");
    assert!(
        !out.status.success(),
        "doctor must fail when riz.toml is missing"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not found"),
        "stdout must explain the missing config; got: {stdout}"
    );
    assert!(
        stdout.contains("riz init"),
        "stdout must hint at `riz init`; got: {stdout}"
    );
}

#[test]
fn doctor_passes_on_valid_scaffold() {
    // Scaffold a TS project via `riz init`, then doctor it.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    let scaffold = Command::new(riz_binary())
        .args(["init", "typescript-http"])
        .arg(&target)
        .output()
        .expect("spawn riz init");
    assert!(scaffold.status.success(), "init must succeed");

    let out = Command::new(riz_binary())
        .args(["doctor"])
        .current_dir(&target)
        .output()
        .expect("spawn riz doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // bun is required for typescript-http; if it's missing we expect a
    // failure rather than a pass — that's still correct doctor behavior.
    let bun_present = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok();
    if bun_present {
        assert!(
            out.status.success(),
            "doctor must pass on a fresh scaffold with bun present.\nstdout: {stdout}\nstderr: {stderr}"
        );
        for label in [
            "riz.toml present",
            "riz.toml parses",
            "riz.toml validates",
            "bun on PATH",
            "function `hello` handler",
            "configured port free",
            "All checks passed",
        ] {
            assert!(
                stdout.contains(label),
                "stdout must include the line `{label}`; got: {stdout}"
            );
        }
    } else {
        // bun missing → expect a clear failure with the install hint.
        assert!(!out.status.success(), "doctor must fail when bun missing");
        assert!(
            stdout.contains("bun on PATH") && stdout.contains("bun.sh/install"),
            "stdout must surface the missing-bun failure + install hint; got: {stdout}"
        );
    }
}

#[test]
fn doctor_flags_missing_handler_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = tmp.path().join("riz.toml");
    std::fs::write(
        &cfg,
        r#"
[server]
port = 3000
host = "127.0.0.1"

[function.ghost]
runtime = "bun"
handler = "does_not_exist.handler"
timeout_ms = 1000
concurrency = 1
"#,
    )
    .expect("write toml");

    let out = Command::new(riz_binary())
        .args(["doctor"])
        .current_dir(tmp.path())
        .output()
        .expect("spawn riz doctor");
    assert!(
        !out.status.success(),
        "doctor must fail when a handler file is missing"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("function `ghost` handler") && stdout.contains("file not found"),
        "stdout must surface the missing-handler failure; got: {stdout}"
    );
}

#[test]
fn doctor_flags_port_in_use_by_non_riz() {
    // Bind a port with a non-riz listener, then point doctor at it.
    let port = pick_free_port();
    let _hold = TcpListener::bind(("127.0.0.1", port)).expect("bind hold port");

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = tmp.path().join("riz.toml");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let handler = workspace
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
handler = "{handler}"
timeout_ms = 1000
concurrency = 1
"#
        ),
    )
    .expect("write toml");

    let out = Command::new(riz_binary())
        .args(["doctor"])
        .current_dir(tmp.path())
        .output()
        .expect("spawn riz doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The hold is a raw TCP socket, not riz — doctor's /_riz/health probe
    // must time out and conclude "not riz." Outcome: failure.
    assert!(
        !out.status.success(),
        "doctor must fail when a non-riz process holds the configured port"
    );
    assert!(
        stdout.contains("configured port free") && stdout.contains("is in use"),
        "stdout must surface the port-in-use failure; got: {stdout}"
    );
}
