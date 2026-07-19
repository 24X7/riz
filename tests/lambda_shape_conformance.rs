//! Lambda-shape conformance — the static half of R1's public guarantee:
//! every example is authored as a Lambda handler; authors never write an
//! event loop. (Spec: docs/superpowers/specs/2026-07-19-lambda-shape-purity-
//! and-wasm-capability-suite-design.html. The behavioral half is the parity
//! matrix + template boot smoke.)
//!
//! Scope: the handler-source trees — `templates/*/`, `examples/lambdas/*/`,
//! `examples/ai-chat/`, `examples/typescript-todo/`. Demo/smoke
//! tooling (examples/demo.py, smoke-all.sh) is outside scope by construction.
//! Wire tokens may exist only under src/, assets/, crates/riz-wasm/, and
//! tests/fixtures/ — never in code a user is meant to copy.

use std::fs;
use std::path::{Path, PathBuf};

/// Banned wire tokens by file extension. A hit means the file hand-writes the
/// transport instead of authoring a handler.
const BANNED: &[(&str, &[&str])] = &[
    ("ts", &["process.stdin", "readline("]),
    ("mjs", &["process.stdin", "readline("]),
    ("js", &["process.stdin", "readline("]),
    ("py", &["sys.stdin"]),
    ("rs", &["io::stdin", "wasm_import_module"]),
];

/// Envelope field names are banned in every scanned extension.
const BANNED_EVERYWHERE: &[&str] = &["__riz_deadline_ms", "__riz_function_name"];

/// Allowlist: (path suffix, token, justification). Every entry MUST carry a
/// justification string — the same discipline as SAFETY.md's allow comments.
/// Starts empty; grow it only for code that legitimately teaches or drives the
/// wire (none known today).
const ALLOW: &[(&str, &str, &str)] = &[];

const SCANNED_EXTS: &[&str] = &["ts", "mjs", "js", "py", "rs", "go"];
const SKIP_DIRS: &[&str] = &["target", "node_modules", "__pycache__", ".git", "dist"];

#[test]
fn examples_author_lambda_handlers_never_the_wire() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut roots: Vec<PathBuf> = vec![
        root.join("examples/ai-chat"),
        root.join("examples/typescript-todo"),
    ];
    let lambdas = root.join("examples/lambdas");
    for entry in fs::read_dir(&lambdas).expect("examples/lambdas must exist") {
        let entry = entry.expect("readable dir entry");
        if entry.path().is_dir() {
            roots.push(entry.path());
        }
    }
    // Templates are held to the same authoring contract as examples.
    for entry in fs::read_dir(root.join("templates")).expect("templates/ must exist") {
        let entry = entry.expect("readable dir entry");
        if entry.path().is_dir() {
            roots.push(entry.path());
        }
    }

    let mut violations = Vec::new();
    for dir in roots {
        scan_dir(&dir, root, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "wire tokens found in example sources — author a handler; the adapter/shim owns the wire:\n{}",
        violations.join("\n")
    );
}

fn scan_dir(dir: &Path, repo_root: &Path, violations: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            scan_dir(&path, repo_root, violations);
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !SCANNED_EXTS.contains(&ext) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .display()
            .to_string();

        let per_ext: &[&str] = BANNED
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, toks)| *toks)
            .unwrap_or(&[]);
        for token in per_ext.iter().chain(BANNED_EVERYWHERE) {
            if allowlisted(&rel, token) {
                continue;
            }
            for (lineno, line) in content.lines().enumerate() {
                if line.contains(token) {
                    violations.push(format!("  {rel}:{}: `{token}`", lineno + 1));
                }
            }
        }
    }
}

fn allowlisted(rel_path: &str, token: &str) -> bool {
    ALLOW.iter().any(|(suffix, tok, justification)| {
        assert!(
            !justification.trim().is_empty(),
            "ALLOW entries require a justification"
        );
        rel_path.ends_with(suffix) && *tok == token
    })
}

/// The scaffold set is a lockstep surface of the RuntimeKind enum, enforced
/// by test rather than prose: one template per variant, names carrying the
/// spec's wording (typescript-bun / typescript-node / python / rust / go /
/// wasm-rust), each declaring the runtime it claims.
#[test]
fn template_set_maps_one_to_one_onto_runtime_kinds() {
    // variant (serde name) → template dir
    const MAP: &[(&str, &str)] = &[
        ("bun", "typescript-bun"),
        ("node", "typescript-node"),
        ("python", "python"),
        ("rust", "rust"),
        ("go", "go"),
        ("wasm", "wasm-rust"),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Every mapped template exists and declares the runtime it claims.
    for (variant, template) in MAP {
        let dir = root.join("templates").join(template);
        assert!(
            dir.is_dir(),
            "templates/{template} missing for runtime {variant}"
        );
        let cfg = fs::read_to_string(dir.join("riz.toml"))
            .unwrap_or_else(|e| panic!("templates/{template}/riz.toml unreadable: {e}"));
        assert!(
            cfg.contains(&format!("runtime = \"{variant}\"")),
            "templates/{template}/riz.toml must declare runtime = \"{variant}\""
        );
    }

    // The on-disk template dirs are exactly the mapped set (no extras, none
    // missing) …
    let mut on_disk: Vec<String> = fs::read_dir(root.join("templates"))
        .expect("templates/ exists")
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = MAP.iter().map(|(_, t)| t.to_string()).collect();
    expected.sort();
    assert_eq!(
        on_disk, expected,
        "templates/ dirs must match the RuntimeKind map"
    );

    // … and BUILTINS advertises exactly the same six template rows.
    let mut advertised: Vec<String> = riz::template_fetch::BUILTINS
        .iter()
        .filter(|(_, subdir, ..)| riz::template_fetch::is_template_row(subdir))
        .map(|(name, ..)| name.to_string())
        .collect();
    advertised.sort();
    assert_eq!(
        advertised, expected,
        "riz new --list templates must match the map"
    );
}
