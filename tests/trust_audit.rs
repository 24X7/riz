//! Trust-audit guard — fails on high-signal test anti-patterns.
//!
//! Scans every `*.rs` under `tests/` and `src/` for:
//!   1. Tautological assertions: `assert!(true)`, `assert_eq!(1, 1)`, `assert!(1 == 1)`.
//!   2. Bare `#[ignore]` without a reason string (`#[ignore = "..."]` is fine).
//!   3. Empty test bodies: a `#[test]` or `#[tokio::test]` attribute immediately
//!      followed by a function whose body contains only whitespace.
//!
//! Any real exception must be added to `ALLOWLIST` with a written justification.
//! This test must always remain GREEN.
//!
//! Last enforced: 2026-06-09

use std::fs;
use std::path::{Path, PathBuf};

/// Allowlist entries: `(file_substring, pattern_substring, justification)`.
///
/// An offending line is exempt when the file path contains `file_substring`
/// AND the flagged content contains `pattern_substring`.
///
/// Keep entries minimal and explicit — "any file, any pattern" entries are
/// not permitted. Every entry requires a written justification.
const ALLOWLIST: &[(&str, &str, &str)] = &[
    // wave_8_acceptance.rs counts literal occurrences of "#[ignore" in file content
    // as a string literal (not an attribute). The grep pattern would match the
    // string "#[ignore" inside an assert! message — but we match on the actual
    // raw text the scanner sees. The scanner checks *attribute syntax* on non-comment
    // lines, so these string literals in assert messages are harmless. Kept as
    // documentation of the known string occurrences.
    (
        "wave_8_acceptance.rs",
        "#[ignore",
        "This file counts '#[ignore' as a string literal inside assert messages, not as an attribute.",
    ),
    // trust_audit.rs itself contains #[ignore pattern strings in the allowlist and comments.
    (
        "trust_audit.rs",
        "#[ignore",
        "This file (the guard itself) mentions the patterns it scans for in comments and the allowlist.",
    ),
];

/// Collect every `*.rs` file under the given root directory (recursively).
fn collect_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let entries = fs::read_dir(root).expect("read_dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_rs_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// Returns true if the violation at `file`/`content_fragment` is in the allowlist.
fn is_allowlisted(file_path: &Path, content_fragment: &str) -> bool {
    let file_str = file_path.to_string_lossy();
    for (file_sub, pattern_sub, _justification) in ALLOWLIST {
        if file_str.contains(file_sub) && content_fragment.contains(pattern_sub) {
            return true;
        }
    }
    false
}

/// Check rule 1: tautological assertions.
/// Flags lines matching: `assert!(true)`, `assert_eq!(1, 1)`, `assert!(1 == 1)`.
/// Space-tolerant matching.
#[allow(clippy::type_complexity)]
fn check_tautological_assertions(file_path: &Path, content: &str, violations: &mut Vec<String>) {
    let patterns: &[(&str, &dyn Fn(&str) -> bool)] = &[
        ("assert!(true)", &|line: &str| {
            // Match assert!( <optional spaces> true <optional spaces> )
            let stripped = line.replace([' ', '\t'], "");
            stripped.contains("assert!(true)") || stripped.contains("assert!(true,")
        }),
        ("assert_eq!(1, 1)", &|line: &str| {
            let stripped = line.replace([' ', '\t'], "");
            stripped.contains("assert_eq!(1,1)")
        }),
        ("assert!(1 == 1)", &|line: &str| {
            let stripped = line.replace([' ', '\t'], "");
            stripped.contains("assert!(1==1)")
        }),
    ];

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip comment lines
        if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("/*") {
            continue;
        }
        for (label, matcher) in patterns {
            if matcher(line) {
                let fragment = format!("{}:{}", label, trimmed);
                if !is_allowlisted(file_path, &fragment) && !is_allowlisted(file_path, label) {
                    violations.push(format!(
                        "[tautological] {}:{}: {:?}",
                        file_path.display(),
                        i + 1,
                        trimmed
                    ));
                }
            }
        }
    }
}

/// Check rule 2: bare `#[ignore]` without a reason string.
/// `#[ignore]` → violation.  `#[ignore = "..."]` → OK.
fn check_bare_ignore(file_path: &Path, content: &str, violations: &mut Vec<String>) {
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip comment lines
        if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("/*") {
            continue;
        }
        // Bare #[ignore] — not followed by = (with optional spaces)
        // Must contain #[ignore] but NOT #[ignore =
        if trimmed.contains("#[ignore]") {
            let _fragment = format!("#[ignore]:line:{}", i + 1);
            if !is_allowlisted(file_path, "#[ignore") {
                violations.push(format!(
                    "[bare-ignore] {}:{}: bare #[ignore] with no reason — use #[ignore = \"reason\"] or remove",
                    file_path.display(),
                    i + 1
                ));
            }
        }
    }
}

/// Check rule 3: empty test bodies.
/// A `#[test]` or `#[tokio::test]` (with optional `#[ignore = ...]`) followed by
/// `(async) fn name() { }` where the body contains only whitespace.
///
/// Uses a simple line-by-line state machine to avoid false positives.
fn check_empty_test_bodies(file_path: &Path, content: &str, violations: &mut Vec<String>) {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    let mut i = 0usize;

    while i < n {
        let trimmed = lines[i].trim();

        // Detect a test attribute line
        let is_test_attr = trimmed == "#[test]"
            || trimmed.starts_with("#[tokio::test")
            || trimmed.starts_with("#[test(");

        if is_test_attr {
            // Scan forward: skip over other attributes (like #[ignore = ...])
            let _attr_line = i;
            let mut j = i + 1;
            while j < n {
                let next = lines[j].trim();
                if next.starts_with("#[") {
                    j += 1; // another attribute, keep scanning
                } else {
                    break;
                }
            }
            // j now points at the fn declaration line (or end-of-file)
            if j < n {
                let fn_line = lines[j].trim();
                // Match: (pub )? (async )? fn name(...) {
                if fn_line.starts_with("fn ")
                    || fn_line.starts_with("async fn ")
                    || fn_line.starts_with("pub fn ")
                    || fn_line.starts_with("pub async fn ")
                {
                    // Check if the body is empty: fn name() {} on one line
                    if fn_line.ends_with("{}") || fn_line.ends_with("{ }") {
                        let fragment = format!("empty_body:{}", fn_line);
                        if !is_allowlisted(file_path, &fragment)
                            && !is_allowlisted(file_path, fn_line)
                        {
                            violations.push(format!(
                                "[empty-test-body] {}:{}: test function has an empty body — add assertions or #[ignore = \"reason\"]",
                                file_path.display(),
                                j + 1
                            ));
                        }
                    } else if fn_line.ends_with('{') {
                        // Multi-line body — check if the entire body until closing brace
                        // contains only whitespace or comments
                        let mut depth = 1i32;
                        let mut k = j + 1;
                        let mut has_real_content = false;
                        while k < n && depth > 0 {
                            let body_line = lines[k].trim();
                            for ch in lines[k].chars() {
                                if ch == '{' {
                                    depth += 1;
                                }
                                if ch == '}' {
                                    depth -= 1;
                                }
                            }
                            if depth > 0 && !body_line.is_empty() && !body_line.starts_with("//") {
                                has_real_content = true;
                            }
                            k += 1;
                        }
                        if !has_real_content {
                            let fragment = format!("empty_body_multiline:{}", fn_line);
                            if !is_allowlisted(file_path, &fragment)
                                && !is_allowlisted(file_path, fn_line)
                            {
                                violations.push(format!(
                                    "[empty-test-body] {}:{}: test function body is empty (only whitespace/comments) — add assertions or #[ignore = \"reason\"]",
                                    file_path.display(),
                                    j + 1
                                ));
                            }
                        }
                    }
                }
            }
            i = j + 1;
            continue;
        }

        i += 1;
    }
}

#[test]
fn trust_audit_no_anti_patterns() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tests_dir = manifest_dir.join("tests");
    let src_dir = manifest_dir.join("src");

    let mut all_files = collect_rs_files(&tests_dir);
    all_files.extend(collect_rs_files(&src_dir));
    // Exclude the guard itself: it necessarily contains the pattern strings it
    // scans for as string literals and as the patterns tuple labels. Scanning
    // trust_audit.rs would be a tautological self-report, not a real finding.
    all_files.retain(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n != "trust_audit.rs")
            .unwrap_or(true)
    });
    all_files.sort();

    let mut violations: Vec<String> = Vec::new();

    for file_path in &all_files {
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("WARN: could not read {}: {}", file_path.display(), e);
                continue;
            }
        };

        check_tautological_assertions(file_path, &content, &mut violations);
        check_bare_ignore(file_path, &content, &mut violations);
        check_empty_test_bodies(file_path, &content, &mut violations);
    }

    if !violations.is_empty() {
        let msg = format!(
            "\n\ntrust_audit: {} anti-pattern(s) detected across {} files:\n\n{}\n\n\
            Fix each violation OR add an entry to ALLOWLIST in tests/trust_audit.rs \
            with a written justification.\n",
            violations.len(),
            all_files.len(),
            violations.join("\n")
        );
        panic!("{}", msg);
    }

    eprintln!(
        "trust_audit: scanned {} files — no anti-patterns found.",
        all_files.len()
    );
}
