//! Guard: makes "the docs run" a CI contract.
//!
//! Parses ```bash``` fenced blocks in `CONTRIBUTING.md` and:
//!
//!  1. EXECUTES every `cargo run [--quiet] -- <ARGS>` line that carries a
//!     `# @verify exit=<N>` / `# @verify exit!=0` marker — rewritten to call
//!     the already-built `target/debug/riz` directly (no `cargo run` shell-out,
//!     which would be far too slow). Asserts the exit status matches the
//!     documented expectation.
//!
//!  2. SHAPE-CHECKS (does not execute) the heavy / networked commands — anything
//!     with `wrk`, `rustup`, `brew`, `bun install`, `curl`, a trailing ` &`, or
//!     a server boot (`cargo build` / `cargo run -- ... run`). For these we only
//!     assert the subcommand name is known AND that global flags
//!     (`--dev`/`--config`/`--port`/`--log-level`) appear BEFORE the subcommand.
//!     This catches a future edit that puts a global flag after the subcommand
//!     (which would fail to parse at runtime).
//!
//! Designed to be FAST and NON-FLAKY: total runtime a couple seconds.

use std::path::PathBuf;
use std::process::Command;

fn riz_binary() -> PathBuf {
    // Mirrors tests/cli_mcp_inspect.rs::riz_binary — resolve the binary that
    // nextest already built for this run.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn contributing_md() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("CONTRIBUTING.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Global flags that MUST precede the subcommand.
const GLOBAL_FLAGS: &[&str] = &["--dev", "--config", "--port", "--log-level"];

/// Every subcommand name the CLI accepts (clap `Commands` + `mcp`).
const KNOWN_SUBCOMMANDS: &[&str] =
    &["run", "validate", "routes", "deploy", "mcp", "doctor", "init"];

/// A single ```bash``` line, with the `# @verify ...` marker (if any) that
/// preceded it on its own line.
struct DocCommand {
    /// The raw command line (marker stripped).
    line: String,
    /// `Some("exit=0")` / `Some("exit!=0")` when the previous line was a
    /// `# @verify ...` marker; `None` otherwise (shape-check only).
    verify: Option<String>,
}

/// Extract every command line inside ```bash fences. A line beginning with
/// `# @verify` attaches to the NEXT non-empty command line.
fn parse_bash_commands(md: &str) -> Vec<DocCommand> {
    let mut out = Vec::new();
    let mut in_bash = false;
    let mut pending_verify: Option<String> = None;

    for raw in md.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") {
            // Toggle: open only on a ```bash fence; any ``` closes.
            if in_bash {
                in_bash = false;
            } else {
                in_bash = trimmed.starts_with("```bash");
            }
            pending_verify = None;
            continue;
        }
        if !in_bash {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# @verify") {
            pending_verify = Some(rest.trim().to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            // Comment or blank line inside the block — does not consume a
            // pending marker (markers sit directly above the command).
            continue;
        }
        out.push(DocCommand {
            line: trimmed.to_string(),
            verify: pending_verify.take(),
        });
    }
    out
}

/// Pull the `cargo run [--quiet] -- <ARGS...>` tail out of a line, returning the
/// post-`--` argv. Returns None if the line is not a `cargo run -- ...` invocation.
fn cargo_run_args(line: &str) -> Option<Vec<String>> {
    // Strip a leading `RUST_LOG=... ` / `FOO=bar ` env prefix.
    let mut rest = line;
    loop {
        let mut chars = rest.char_indices();
        if let Some((_, c)) = chars.next() {
            if c.is_ascii_uppercase() || c == '_' {
                // Possible VAR=val prefix: VAR chars then '=' then a token + space.
                if let Some(eq) = rest.find('=') {
                    let space = rest.find(' ');
                    if let Some(sp) = space {
                        if eq < sp
                            && rest[..eq].chars().all(|c| c.is_ascii_uppercase() || c == '_')
                        {
                            rest = rest[sp + 1..].trim_start();
                            continue;
                        }
                    }
                }
            }
        }
        break;
    }

    let toks: Vec<&str> = rest.split_whitespace().collect();
    if toks.first() != Some(&"cargo") || toks.get(1) != Some(&"run") {
        return None;
    }
    // Find the `--` separator.
    let sep = toks.iter().position(|t| *t == "--")?;
    Some(toks[sep + 1..].iter().map(|s| s.to_string()).collect())
}

/// True if the line is a heavy/networked command we only shape-check.
fn is_shape_check_only(line: &str) -> bool {
    let heavy = [
        "wrk", "rustup", "brew", "bun install", "curl", "cargo build", "cargo install",
        "cargo watch", "cargo fmt", "cargo clippy", "cargo flamegraph", "python3",
    ];
    if heavy.iter().any(|h| line.contains(h)) {
        return true;
    }
    if line.trim_end().ends_with('&') {
        return true;
    }
    // Server boot: `cargo run -- ... run`
    if let Some(args) = cargo_run_args(line) {
        if args.last().map(String::as_str) == Some("run") {
            return true;
        }
    }
    false
}

/// Index of the first arg that is a known subcommand.
fn subcommand_index(args: &[String]) -> Option<usize> {
    args.iter().position(|a| KNOWN_SUBCOMMANDS.contains(&a.as_str()))
}

/// Assert global flags appear before the subcommand for a `cargo run -- <args>`
/// line. Returns the subcommand name if one is present.
fn assert_global_flags_precede_subcommand(line: &str, args: &[String]) {
    let Some(sub_idx) = subcommand_index(args) else {
        // No subcommand (e.g. `--version`) — nothing to order against.
        return;
    };
    // Any global flag appearing AT or AFTER the subcommand is a bug.
    for (i, arg) in args.iter().enumerate() {
        if i >= sub_idx && GLOBAL_FLAGS.contains(&arg.as_str()) {
            panic!(
                "CONTRIBUTING.md command has a global flag '{arg}' at/after the \
                 subcommand '{}': `{line}`. Global flags ({}) MUST precede the \
                 subcommand.",
                args[sub_idx],
                GLOBAL_FLAGS.join(", ")
            );
        }
    }
}

#[test]
fn contributing_commands_are_runnable() {
    let md = contributing_md();
    let cmds = parse_bash_commands(&md);
    assert!(!cmds.is_empty(), "no bash commands parsed from CONTRIBUTING.md");

    let mut verified = 0usize;
    let mut shape_checked = 0usize;

    for cmd in &cmds {
        let line = &cmd.line;

        // ── EXECUTE: @verify-marked, non-heavy cargo-run CLI commands ───────
        if let Some(expect) = &cmd.verify {
            let args = cargo_run_args(line).unwrap_or_else(|| {
                panic!("@verify command is not a `cargo run -- ...` line: `{line}`")
            });
            assert!(
                !is_shape_check_only(line),
                "@verify command must be safe/fast (no server boot / network): `{line}`"
            );
            // Global-flag ordering must be correct or the real CLI would reject it.
            assert_global_flags_precede_subcommand(line, &args);

            let output = Command::new(riz_binary())
                .args(&args)
                .output()
                .unwrap_or_else(|e| panic!("spawn riz {args:?}: {e}"));
            let code = output.status.code();

            match expect.split_whitespace().next() {
                Some("exit=0") => assert!(
                    output.status.success(),
                    "expected exit 0 for `{line}` (args {args:?}); got {code:?}.\nstderr: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
                Some("exit!=0") => assert!(
                    !output.status.success(),
                    "expected nonzero exit for `{line}` (args {args:?}); got success.\n\
                     stdout: {}",
                    String::from_utf8_lossy(&output.stdout)
                ),
                other => panic!("unrecognized @verify marker {other:?} on `{line}`"),
            }
            verified += 1;
            continue;
        }

        // ── SHAPE-CHECK ONLY: heavy / networked, or any other cargo-run line ─
        if let Some(args) = cargo_run_args(line) {
            // Known subcommand (if any) + global-flag ordering.
            if let Some(idx) = subcommand_index(&args) {
                assert!(
                    KNOWN_SUBCOMMANDS.contains(&args[idx].as_str()),
                    "unknown subcommand in `{line}`"
                );
            } else {
                // A `cargo run -- ...` with no subcommand is only OK for the
                // clap-auto flags (--version/--help/-V/-h).
                assert!(
                    args.iter().any(|a| matches!(
                        a.as_str(),
                        "--version" | "--help" | "-V" | "-h"
                    )),
                    "`cargo run --` line with no known subcommand and no \
                     --version/--help: `{line}`"
                );
            }
            assert_global_flags_precede_subcommand(line, &args);
            shape_checked += 1;
        }
        // Non-cargo-run lines (curl, wrk, rustup, etc.) need no further check
        // here — the toolchain/install/curl blocks are prose-verified.
    }

    // Sanity: we actually exercised the marked commands. The @verify block in
    // CONTRIBUTING has 5 markers (version/routes/validate/doctor/mcp-inspect).
    assert!(
        verified >= 5,
        "expected at least 5 @verify commands executed, ran {verified}"
    );
    assert!(
        shape_checked >= 1,
        "expected at least one shape-checked cargo-run command, saw {shape_checked}"
    );
    eprintln!("docs guard: executed {verified} @verify commands, shape-checked {shape_checked} cargo-run commands");
}
