//! Repository cleanliness guard.
//!
//! Enforces that committed `docs/**` and a source subset stay free of:
//!   1. AI-slop text markers
//!   2. Merge conflict scars
//!   3. Stale session-state dumps in docs/status/
//!   4. `docs/superpowers/plans/*.md` that lack a matching spec OR a Status marker
//!
//! Rules are tight and low-false-positive. See the ALLOWLIST for known-good
//! exceptions.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Paths that are **allowed** to contain a specific slop pattern.
/// Format: (file_suffix_or_name, pattern_substring_lower).
///
/// Keep this list short and specific. If a doc must legitimately show a pattern
/// as an example, add an entry here rather than weakening the regex.
const ALLOWLIST: &[(&str, &str)] = &[
    // The claims-truth plan itself quotes slop markers as examples of what to
    // look for — those appearances are intentional.
    ("2026-06-09-claims-truth-and-ai-substrate.md", "as an ai"),
    (
        "2026-06-09-claims-truth-and-ai-substrate.md",
        "todo(claude)",
    ),
    ("2026-06-09-claims-truth-and-ai-substrate.md", "here's the"),
    ("2026-06-09-claims-truth-and-ai-substrate.md", "here is the"),
    // The repo-cleanliness plan describes the very markers this test enforces.
    ("repo_cleanliness.rs", "as an ai"),
    ("repo_cleanliness.rs", "todo(claude)"),
    ("repo_cleanliness.rs", "here's the"),
    ("repo_cleanliness.rs", "here is the"),
    ("repo_cleanliness.rs", "i'm sorry, but"),
    ("repo_cleanliness.rs", "let me know if"),
];

/// Roots to scan for AI-slop and merge-conflict markers.
/// Tuples are (dir, extension). Source files are scanned only for a subset.
const SCAN_ROOTS: &[(&str, &str)] = &[("docs", "md"), ("tests", "rs"), ("src", "rs")];

/// Only check docs/status/ — no other location is expected to hold these.
const STATUS_DIR: &str = "docs/status";

/// Cutoff: session-state files older than this date prefix are stale.
const STALE_CUTOFF: &str = "2026-06-01";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_files(root: &str, ext: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let base = Path::new(root);
    if !base.exists() {
        return result;
    }
    collect_recursive(base, ext, &mut result);
    result.sort();
    result
}

fn collect_recursive(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

fn file_name_str(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn is_allowlisted(path: &Path, pattern_lower: &str) -> bool {
    let name = file_name_str(path);
    ALLOWLIST
        .iter()
        .any(|(suffix, pat)| name.ends_with(suffix) && *pat == pattern_lower)
}

/// Read file; return empty string on any IO error so the check is a no-op.
fn read_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule 1 — AI-slop text markers
// ---------------------------------------------------------------------------

/// Case-insensitive substring patterns that signal AI-generated filler text.
///
/// The match is anchored so "as an AI" must appear as a recognisable phrase,
/// but we do NOT require line-start for mid-doc variants (merge scars are
/// always line-start, so they're handled separately).
const SLOP_PATTERNS: &[(&str, &str)] = &[
    ("as an ai", "phrase 'as an AI' (AI self-reference)"),
    (
        "i'm sorry, but",
        "phrase 'I\\'m sorry, but' (AI apology opener)",
    ),
    ("todo(claude)", "marker 'TODO(claude)' (AI-addressed TODO)"),
];

/// These must appear only at the **start of a line** (after trimming whitespace)
/// to avoid false-positives inside code or prose.
const LINE_START_SLOP: &[(&str, &str)] = &[
    ("here's the ", "filler opener 'Here's the …' at line start"),
    (
        "here is the ",
        "filler opener 'Here is the …' at line start",
    ),
    (
        "let me know if",
        "assistant sign-off 'Let me know if…' at line start",
    ),
];

fn check_slop(path: &Path, violations: &mut Vec<String>) {
    let content = read_lossy(path);
    let lower = content.to_lowercase();
    let display = path.display();

    // Anywhere-in-file patterns
    for (pat, desc) in SLOP_PATTERNS {
        if lower.contains(pat) && !is_allowlisted(path, pat) {
            violations.push(format!("{display}: contains {desc}"));
        }
    }

    // Line-start patterns
    for (line_no, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim().to_lowercase();
        for (pat, desc) in LINE_START_SLOP {
            if trimmed.starts_with(pat) && !is_allowlisted(path, pat) {
                violations.push(format!("{display}:{}: contains {desc}", line_no + 1));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — Merge conflict scars
// ---------------------------------------------------------------------------

fn check_merge_conflicts(path: &Path, violations: &mut Vec<String>) {
    let content = read_lossy(path);
    let display = path.display();
    for (line_no, line) in content.lines().enumerate() {
        if line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line.starts_with("=======")
        {
            violations.push(format!(
                "{display}:{}: merge conflict scar ({:?})",
                line_no + 1,
                &line[..7.min(line.len())]
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3 — Stale session-state dumps
// ---------------------------------------------------------------------------

fn check_stale_status_dumps(violations: &mut Vec<String>) {
    let dir = Path::new(STATUS_DIR);
    if !dir.exists() {
        return; // no status dir — clean
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = file_name_str(&path).to_lowercase();
        // Look for session-state pattern with a date prefix older than cutoff
        if !name.contains("session") && !name.contains("state") && !name.contains("session-state") {
            continue;
        }
        // Date prefix is YYYY-MM-DD at the start
        if name.as_str() < STALE_CUTOFF {
            violations.push(format!(
                "{}: stale session-state dump (date {} < cutoff {}); delete it",
                path.display(),
                &name[..10.min(name.len())],
                STALE_CUTOFF
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 4 — Plans without spec or status marker
// ---------------------------------------------------------------------------

/// Plans that are exempt from the spec-or-status requirement because they are
/// the currently active plan being executed.
const ACTIVE_PLANS: &[&str] = &["2026-06-09-claims-truth-and-ai-substrate.md"];

/// Plans whose spec lives under a slightly different date (near-match).
/// Format: (plan_file_name, spec_file_name).
const MANUAL_SPEC_OVERRIDES: &[(&str, &str)] = &[
    // The phase-1 plan was written the day after the lambda-host-design spec.
    (
        "2026-05-19-osbox-phase1.md",
        "2026-05-18-lambda-host-design.md",
    ),
];

fn spec_exists_for_plan(plan_name: &str) -> bool {
    // Manual overrides first
    for (p, s) in MANUAL_SPEC_OVERRIDES {
        if *p == plan_name {
            let spec_path = PathBuf::from("docs/superpowers/specs").join(s);
            return spec_path.exists();
        }
    }

    // Auto-match: same date prefix, spec dir has a file starting with same YYYY-MM-DD
    let date_prefix = &plan_name[..10]; // "YYYY-MM-DD"
    let specs_dir = Path::new("docs/superpowers/specs");
    let Ok(rd) = fs::read_dir(specs_dir) else {
        return false;
    };
    for entry in rd.flatten() {
        let spec_name = entry.file_name();
        let spec_str = spec_name.to_string_lossy();
        if spec_str.starts_with(date_prefix) && spec_str.ends_with(".md") {
            return true;
        }
    }
    false
}

fn plan_has_status_marker(plan_path: &Path) -> bool {
    let content = read_lossy(plan_path);
    let lower = content.to_lowercase();
    // Accept "> status:", "status: archived", "status: superseded", "status: completed"
    lower.contains("> status:")
        || lower.contains("status: archived")
        || lower.contains("status: superseded")
        || lower.contains("status: completed")
}

fn check_plans_coverage(violations: &mut Vec<String>) {
    let plans_dir = Path::new("docs/superpowers/plans");
    if !plans_dir.exists() {
        return;
    }
    let Ok(rd) = fs::read_dir(plans_dir) else {
        return;
    };
    let mut plans: Vec<_> = rd.flatten().collect();
    plans.sort_by_key(|e| e.file_name());

    for entry in plans {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = file_name_str(&path);

        // Skip active/exempt plans
        if ACTIVE_PLANS.contains(&name) {
            continue;
        }

        // OK if there is a matching spec
        if spec_exists_for_plan(name) {
            continue;
        }

        // OK if the plan itself carries a status marker
        if plan_has_status_marker(&path) {
            continue;
        }

        violations.push(format!(
            "{}: plan has no matching spec in docs/superpowers/specs/ and no '> Status:' marker; \
             add one or create the spec",
            path.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// Main test entry point
// ---------------------------------------------------------------------------

#[test]
fn repo_is_clean() {
    let mut violations: Vec<String> = Vec::new();

    // Rules 1 + 2 — slop and merge conflicts across SCAN_ROOTS
    for (root, ext) in SCAN_ROOTS {
        for path in collect_files(root, ext) {
            check_slop(&path, &mut violations);
            check_merge_conflicts(&path, &mut violations);
        }
    }

    // Rule 3 — stale session-state dumps
    check_stale_status_dumps(&mut violations);

    // Rule 4 — plans coverage
    check_plans_coverage(&mut violations);

    if violations.is_empty() {
        return;
    }

    let mut msg = format!(
        "\n\n=== repo_cleanliness: {} violation(s) ===\n\n",
        violations.len()
    );
    for v in &violations {
        let _ = writeln!(msg, "  • {v}");
    }
    msg.push_str(
        "\nTo fix:\n\
         • AI-slop markers: remove or rewrite the flagged text\n\
         • Merge scars: resolve or drop the conflict markers\n\
         • Stale status dumps: `git rm` the file\n\
         • Plans without spec/status: add `> Status: archived` (or create the spec)\n\
         • If a match is a false-positive: add an entry to ALLOWLIST in tests/repo_cleanliness.rs\n",
    );
    panic!("{msg}");
}
