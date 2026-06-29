//! `riz init <spec>` — scaffolds a project by FETCHING a template from git,
//! never from embedded strings.
//!
//! These tests are hermetic: built-in names resolve through `RIZ_TEMPLATE_REPO`
//! pointed at this checkout, so no network is touched. One test exercises the
//! real `git clone` code path against a local `file://` repo.
//!
//! Run: `cargo nextest run --test cli_init`

use std::path::PathBuf;
use std::process::{Command, Output};

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

/// This checkout — used as the local `RIZ_TEMPLATE_REPO` so built-in template
/// names resolve to the on-disk `templates/` / `examples/` dirs (hermetic).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn assert_riz_available() {
    assert!(
        riz_binary().exists(),
        "riz binary not built at {}; run `cargo build` first",
        riz_binary().display()
    );
}

/// Run `riz init <args>` with the local template repo override. A git identity
/// is provided via env so the `--git` path's `git commit` succeeds even on a
/// host with no global git config (e.g. CI runners).
fn init(args: &[&str]) -> Output {
    Command::new(riz_binary())
        .arg("init")
        .args(args)
        .env("RIZ_TEMPLATE_REPO", repo_root())
        .env("GIT_AUTHOR_NAME", "riz test")
        .env("GIT_AUTHOR_EMAIL", "test@riz.dev")
        .env("GIT_COMMITTER_NAME", "riz test")
        .env("GIT_COMMITTER_EMAIL", "test@riz.dev")
        .output()
        .expect("spawn riz init")
}

// ─────────────────────────── built-in templates ─────────────────────────────

#[test]
fn builtin_templates_scaffold_from_the_git_location() {
    assert_riz_available();
    for (name, must_have) in [
        ("typescript-http", "index.ts"),
        ("python-http", "main.py"),
        ("rust-http", "Cargo.toml"),
        ("nodejs-http", "index.mjs"),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("app");
        let out = init(&[name, target.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "init {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            target.join("riz.toml").is_file(),
            "{name}: riz.toml missing"
        );
        assert!(
            target.join(must_have).is_file(),
            "{name}: expected {must_have}"
        );
        // The scaffolded config must be a valid riz config.
        let cfg_src = std::fs::read_to_string(target.join("riz.toml")).unwrap();
        let cfg: riz::config::Config =
            toml::from_str(&cfg_src).unwrap_or_else(|e| panic!("{name} riz.toml parses: {e}"));
        assert!(!cfg.functions.is_empty(), "{name}: no functions declared");
    }
}

#[test]
fn full_stack_todo_template_brings_api_and_client() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("todo");
    let out = init(&["typescript-todo", target.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "init typescript-todo failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(target.join("api/todos.ts").is_file(), "API handler missing");
    assert!(
        target.join("client/package.json").is_file(),
        "client missing"
    );
    assert!(
        target.join("client/dist/index.html").is_file(),
        "built client missing"
    );
    // Cruft a local copy must NOT drag along.
    assert!(
        !target.join("client/node_modules").exists(),
        "node_modules leaked into the scaffold"
    );
}

// ─────────────────────────── overwrite semantics ────────────────────────────

#[test]
fn refuses_nonempty_dir_without_force_then_overwrites_with_force() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("app");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "mine").unwrap();

    let out = init(&["typescript-http", target.to_str().unwrap()]);
    assert!(!out.status.success(), "must refuse a non-empty dir");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));

    let out = init(&["typescript-http", target.to_str().unwrap(), "--force"]);
    assert!(
        out.status.success(),
        "--force should scaffold into a non-empty dir: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(target.join("riz.toml").is_file());
}

// ─────────────────────────── --list / errors ────────────────────────────────

#[test]
fn list_enumerates_official_templates_including_full_stack() {
    assert_riz_available();
    let out = init(&["--list"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for name in [
        "typescript-http",
        "python-http",
        "rust-http",
        "nodejs-http",
        "typescript-websocket",
        "python-websocket",
        "rust-websocket",
        "typescript-todo",
    ] {
        assert!(stdout.contains(name), "--list missing {name}");
    }
    // It must advertise the bring-your-own-repo path.
    assert!(stdout.contains("owner") && stdout.contains("repo"));
}

#[test]
fn unknown_spec_fails_with_a_helpful_message() {
    assert_riz_available();
    let tmp = tempfile::TempDir::new().unwrap();
    // A bare word that is neither a built-in, a path, nor owner/repo.
    let out = init(&[
        "nope-not-a-template",
        tmp.path().join("x").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--list") || stderr.contains("owner/repo"));
}

// ─────────────────────────── --git ──────────────────────────────────────────

#[test]
fn git_flag_creates_initial_commit() {
    assert_riz_available();
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("SKIP: git not on PATH");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("app");
    let out = init(&["typescript-http", target.to_str().unwrap(), "--git"]);
    assert!(out.status.success(), "init --git failed");

    let log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&target)
        .output()
        .expect("git log");
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("riz init"),
        "expected an initial 'riz init' commit"
    );
}

// ─────────────────────── the real git-clone code path ───────────────────────

#[test]
fn clones_a_template_from_a_real_git_repo() {
    assert_riz_available();
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("SKIP: git not on PATH");
        return;
    }
    // Build a throwaway git repo containing a template, then init from it via a
    // file:// URL — exercising `git clone`, not the local-copy shortcut.
    let src = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(src.path())
            .output()
            .expect("git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(
        src.path().join("riz.toml"),
        "[server]\nport = 3000\n[function.x]\nruntime = \"bun\"\nhandler = \"i.handler\"\n",
    )
    .unwrap();
    std::fs::write(
        src.path().join("i.ts"),
        "export const handler = async () => ({});\n",
    )
    .unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "init"]);

    let dest = tempfile::TempDir::new().unwrap();
    let target = dest.path().join("app");
    let url = format!("file://{}", src.path().display());
    // No RIZ_TEMPLATE_REPO override here — this is a direct git URL spec.
    let out = Command::new(riz_binary())
        .args(["init", &url, target.to_str().unwrap()])
        .output()
        .expect("spawn riz init <git-url>");
    assert!(
        out.status.success(),
        "git clone init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(target.join("riz.toml").is_file());
    assert!(target.join("i.ts").is_file());
    assert!(
        !target.join(".git").exists(),
        ".git must not be carried over"
    );
}
