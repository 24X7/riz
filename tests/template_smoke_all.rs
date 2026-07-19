//! template_smoke_all — the behavioral half of R1's scaffold guarantee:
//! `riz new <template> && build && riz run` serves a real APIGW v2 roundtrip
//! for every one of the six per-runtime templates.
//!
//! ISOLATED nextest binary (like e2e_smoke_all): it scaffolds into tempdirs,
//! builds the compiled legs, and boots one riz per leg — keep it out of the
//! default filter:
//!   cargo nextest run --workspace -E 'not binary(e2e_smoke_all) and not binary(template_smoke_all)'
//!   cargo nextest run --test template_smoke_all
//!
//! Hermetic: templates resolve through RIZ_TEMPLATE_REPO = this checkout.
//! Compiled legs reuse a shared cargo target dir under the workspace target/
//! so repeat runs (and CI caches) skip the dependency rebuild; artifacts are
//! copied to the path the scaffolded riz.toml expects.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).output().is_ok()
}

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

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0")
        .local_addr()
        .expect("local addr")
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

fn rewrite_port(toml_path: &Path, port: u16) {
    let s = std::fs::read_to_string(toml_path).expect("read riz.toml");
    std::fs::write(
        toml_path,
        s.replace("port = 3000", &format!("port = {port}")),
    )
    .expect("write");
}

fn scaffold(name: &str, target: &Path) {
    let out = Command::new(riz_binary())
        .args(["new", name])
        .arg(target)
        .env("RIZ_TEMPLATE_REPO", repo_root())
        .output()
        .expect("spawn riz new");
    assert!(
        out.status.success(),
        "riz new {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Shared cargo target dir per template so dependency builds cache across
/// runs; the built artifact is copied to where the scaffolded riz.toml looks.
fn cached_target_dir(leg: &str) -> PathBuf {
    repo_root().join("target").join("template-smoke").join(leg)
}

fn run_build(cmd: &mut Command, what: &str) {
    let out = cmd.output().unwrap_or_else(|e| panic!("spawn {what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Boot `riz run` in the scaffold dir, hit /hello?name=alice, return the body.
fn boot_and_hello(target: &Path, leg: &str) -> String {
    let port = pick_free_port();
    rewrite_port(&target.join("riz.toml"), port);
    let mut server = Command::new(riz_binary())
        .args(["--log-level", "warn", "run"])
        .current_dir(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz run");
    if !wait_for_ready(port, Duration::from_secs(30)) {
        let _ = server.kill();
        panic!("{leg}: scaffold never became ready");
    }
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/hello?name=alice"))
        .expect("GET /hello");
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    let _ = server.kill();
    let _ = server.wait();
    assert!(status.is_success(), "{leg}: status {status}, body: {body}");
    assert!(
        body.contains("hello, alice"),
        "{leg}: expected 'hello, alice'; got: {body}"
    );
    body
}

#[test]
fn typescript_bun_template_serves() {
    if !have("bun", "--version") {
        eprintln!("SKIP typescript-bun: bun missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("typescript-bun", &target);
    let body = boot_and_hello(&target, "typescript-bun");
    assert!(body.contains("functionName") && body.contains("awsRequestId"));
}

#[test]
fn typescript_node_template_serves() {
    if !have("node", "--version") {
        eprintln!("SKIP typescript-node: node missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("typescript-node", &target);
    let body = boot_and_hello(&target, "typescript-node");
    assert!(body.contains("functionName") && body.contains("awsRequestId"));
}

#[test]
fn python_template_serves() {
    if !have("python3", "--version") {
        eprintln!("SKIP python: python3 missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("python", &target);
    boot_and_hello(&target, "python");
}

#[test]
fn rust_template_builds_and_serves() {
    if !have("cargo", "--version") {
        eprintln!("SKIP rust: cargo missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("rust", &target);
    run_build(
        Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(&target)
            .env("CARGO_TARGET_DIR", cached_target_dir("rust")),
        "cargo build (rust template)",
    );
    let built = cached_target_dir("rust").join("release").join("hello");
    let expected = target.join("target/release/hello");
    std::fs::create_dir_all(expected.parent().expect("parent")).expect("mkdir");
    std::fs::copy(&built, &expected).expect("copy rust artifact");
    boot_and_hello(&target, "rust");
}

#[test]
fn go_template_builds_and_serves() {
    if !have("go", "version") {
        eprintln!("SKIP go: go missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("go", &target);
    run_build(
        Command::new("go")
            .args(["build", "-o", "hello", "."])
            .current_dir(&target),
        "go build (go template)",
    );
    boot_and_hello(&target, "go");
}

#[test]
fn wasm_rust_template_builds_and_serves() {
    if !have("cargo", "--version") || !wasm_target_installed() {
        eprintln!("SKIP wasm-rust: cargo or wasm32-wasip1 target missing");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("app");
    scaffold("wasm-rust", &target);

    // The template depends on riz-wasm from the official repo; patch it to
    // THIS checkout so the smoke test verifies the current shim, offline.
    let cargo_toml = target.join("Cargo.toml");
    let mut manifest = std::fs::read_to_string(&cargo_toml).expect("read Cargo.toml");
    manifest.push_str(&format!(
        "\n[patch.\"https://github.com/24X7/riz\"]\nriz-wasm = {{ path = \"{}\" }}\n",
        repo_root().join("crates/riz-wasm").display()
    ));
    std::fs::write(&cargo_toml, manifest).expect("patch Cargo.toml");

    run_build(
        Command::new("cargo")
            .args(["build", "--release", "--target", "wasm32-wasip1"])
            .current_dir(&target)
            .env("CARGO_TARGET_DIR", cached_target_dir("wasm-rust")),
        "cargo build (wasm-rust template)",
    );
    let built = cached_target_dir("wasm-rust").join("wasm32-wasip1/release/hello.wasm");
    let expected = target.join("target/wasm32-wasip1/release/hello.wasm");
    std::fs::create_dir_all(expected.parent().expect("parent")).expect("mkdir");
    std::fs::copy(&built, &expected).expect("copy wasm artifact");

    let body = boot_and_hello(&target, "wasm-rust");
    assert!(
        body.contains("\"runtime\":\"wasm\"") || body.contains("wasm"),
        "wasm-rust: expected the wasm runtime marker; got: {body}"
    );
}
