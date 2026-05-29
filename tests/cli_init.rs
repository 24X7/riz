//! `riz init <template> <dir>` — Tier-2 #4.
//!
//! Verifies the new init subcommand scaffolds a working project from each
//! built-in template. Tests run the actual riz binary as a subprocess and
//! check that:
//!   - the expected files were created
//!   - file contents are non-empty and match the template
//!   - re-running into a non-empty dir refuses to overwrite
//!
//! Run: `cargo nextest run --test cli_init`

use std::path::PathBuf;
use std::process::Command;

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn assert_riz_available() {
    assert!(
        riz_binary().exists(),
        "riz binary not built at {}; run `cargo build` first",
        riz_binary().display()
    );
}

#[test]
fn init_typescript_http_creates_handler_and_config() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("ts-app");

    let out = Command::new(riz_binary())
        .args(["init", "typescript-http"])
        .arg(&target)
        .output()
        .expect("spawn riz init");
    assert!(
        out.status.success(),
        "riz init failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let index_ts = target.join("index.ts");
    let riz_toml = target.join("riz.toml");
    assert!(index_ts.exists(), "expected {} to exist", index_ts.display());
    assert!(riz_toml.exists(), "expected {} to exist", riz_toml.display());

    let handler_src = std::fs::read_to_string(&index_ts).expect("read index.ts");
    assert!(
        handler_src.contains("export const handler"),
        "index.ts must export handler; got {handler_src}"
    );
    let toml_src = std::fs::read_to_string(&riz_toml).expect("read riz.toml");
    assert!(
        toml_src.contains("runtime = \"bun\""),
        "riz.toml must set runtime = bun; got {toml_src}"
    );
    // The generated riz.toml must parse + validate via the library's own
    // parser — proves we're shipping a working template, not just text.
    let parsed: riz::config::Config = toml::from_str(&toml_src).expect("toml parses");
    parsed.validate().expect("generated config must validate");
}

#[test]
fn init_python_http_creates_handler_and_config() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("py-app");

    let out = Command::new(riz_binary())
        .args(["init", "python-http"])
        .arg(&target)
        .output()
        .expect("spawn riz init");
    assert!(
        out.status.success(),
        "riz init failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let main_py = target.join("main.py");
    let riz_toml = target.join("riz.toml");
    assert!(main_py.exists(), "expected {} to exist", main_py.display());
    assert!(riz_toml.exists(), "expected {} to exist", riz_toml.display());

    let handler_src = std::fs::read_to_string(&main_py).expect("read main.py");
    assert!(
        handler_src.contains("def lambda_handler"),
        "main.py must define lambda_handler; got {handler_src}"
    );
    let toml_src = std::fs::read_to_string(&riz_toml).expect("read riz.toml");
    let parsed: riz::config::Config = toml::from_str(&toml_src).expect("toml parses");
    parsed.validate().expect("generated config must validate");
}

#[test]
fn init_refuses_to_overwrite_existing_file() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("collision");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("riz.toml"), "# pre-existing — must not be clobbered\n").unwrap();

    let out = Command::new(riz_binary())
        .args(["init", "typescript-http"])
        .arg(&target)
        .output()
        .expect("spawn riz init");
    assert!(
        !out.status.success(),
        "riz init must REFUSE to overwrite existing riz.toml"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to overwrite"),
        "stderr must explain refusal; got {stderr}"
    );
    // The pre-existing file is unchanged.
    let preserved = std::fs::read_to_string(target.join("riz.toml")).unwrap();
    assert!(
        preserved.contains("pre-existing"),
        "pre-existing riz.toml must not have been modified"
    );
}

#[test]
fn init_unknown_template_fails_with_helpful_message() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("bogus");

    let out = Command::new(riz_binary())
        .args(["init", "go-http"])
        .arg(&target)
        .output()
        .expect("spawn riz init");
    assert!(!out.status.success(), "unknown template must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown template")
            && stderr.contains("typescript-http")
            && stderr.contains("python-http"),
        "stderr must list available templates; got {stderr}"
    );
}
