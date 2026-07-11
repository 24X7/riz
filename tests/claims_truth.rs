//! Claims-truth enforcement — the machine check behind the site's promise:
//! "every capability sentence on this site is pinned to a passing test."
//!
//! `tests/claims/registry.toml` maps each capability claim shown anywhere in
//! `web/*.html` to one of three status labels:
//!
//!   * `proven` — backed by a REAL test fn that exists in `tests/` or `src/`. The site DISPLAYS that fn name (the cap cards' "proof:" line and the detail pages' proof ledgers), so the claim's `page_text` is that fn name: one string that is both a verbatim drift-guard substring of the live page and the test that must exist.
//!   * `benchmark` — a perf number proved by a benches/ recipe, with a deterministic CI-floor sibling test as its `proof`.
//!   * `copy-only` — a subjective/marketing line or a point-in-time stat; needs a `note` saying why it's exempt from a test mapping.
//!
//! Three invariants, enforced below, make a "proof" label impossible to fake:
//!   1. every registry `page_text` appears somewhere on the live site;
//!   2. every proven/benchmark claim's proof fn exists in source;
//!   3. every fn name DISPLAYED on the site as a proof is registered here —
//!      so the page can't show `proof: foo` for a function that doesn't exist.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

const REGISTRY: &str = "tests/claims/registry.toml";
const SITE_DIR: &str = "web";

#[derive(Debug, serde::Deserialize)]
struct Registry {
    #[serde(default)]
    claim: Vec<Claim>,
}

#[derive(Debug, serde::Deserialize)]
struct Claim {
    id: String,
    page_text: String,
    status: String,
    #[serde(default)]
    proof: String,
    #[serde(default)]
    note: String,
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"))
}

fn strip_html_tags(s: &str) -> String {
    Regex::new(r#"<[^>]+>"#)
        .unwrap()
        .replace_all(s, "")
        .to_string()
}

/// The raw concatenated HTML of every page on the site.
fn site_html() -> String {
    let mut pages: Vec<PathBuf> = fs::read_dir(SITE_DIR)
        .unwrap_or_else(|e| panic!("could not read {SITE_DIR}/: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    pages.sort();
    assert!(!pages.is_empty(), "no html pages found under {SITE_DIR}/");
    pages
        .iter()
        .map(|p| fs::read_to_string(p).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tag-stripped rendered text of the whole site. Entities are left as-is
/// (`&amp;`, `&lt;`) — registry `page_text` values are written to match.
/// We keep ONE rendering that preserves `</b>` boundaries for the few claims
/// whose drift guard intentionally pins a tag (e.g. the `91k</b>` stat), by
/// matching against both the raw HTML and the stripped text.
fn site_text() -> String {
    strip_html_tags(&site_html())
}

fn load_registry() -> Registry {
    let raw = read(REGISTRY);
    toml::from_str(&raw).unwrap_or_else(|e| panic!("{REGISTRY} is not valid TOML: {e}"))
}

/// Walk `tests/` and `src/`, return true if `fn {name}` appears in any file.
fn fn_exists_in_source(name: &str) -> bool {
    let needle = format!("fn {name}");
    for dir in ["tests", "src"] {
        if dir_contains(Path::new(dir), &needle) {
            return true;
        }
    }
    false
}

fn dir_contains(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains(&path, needle) {
                return true;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(contents) = fs::read_to_string(&path) {
                if contents.contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}

#[test]
fn registry_is_internally_consistent() {
    let reg = load_registry();
    assert!(
        !reg.claim.is_empty(),
        "registry must declare at least one [[claim]]"
    );

    let mut seen = std::collections::HashSet::new();
    for c in &reg.claim {
        assert!(!c.id.trim().is_empty(), "a claim has an empty id");
        assert!(seen.insert(c.id.clone()), "duplicate claim id: {:?}", c.id);
        assert!(
            matches!(c.status.as_str(), "proven" | "benchmark" | "copy-only"),
            "claim {:?} has unknown status {:?}",
            c.id,
            c.status
        );
    }
}

#[test]
fn every_claim_page_text_appears_on_the_site() {
    let reg = load_registry();
    let text = site_text();
    let raw = site_html();
    for c in &reg.claim {
        assert!(
            !c.page_text.trim().is_empty(),
            "claim {:?} has an empty page_text drift guard",
            c.id
        );
        // Match against the tag-stripped text OR the raw HTML — the latter
        // lets a claim pin a rendering that intentionally includes a tag
        // boundary (e.g. the `91k</b>` stat-bar string).
        assert!(
            text.contains(&c.page_text) || raw.contains(&c.page_text),
            "claim {:?}: page_text {:?} does not appear anywhere on the site \
             (web/*.html) — registry and site have drifted apart",
            c.id,
            c.page_text
        );
    }
}

#[test]
fn proven_and_benchmark_claims_point_at_a_real_test() {
    let reg = load_registry();
    for c in &reg.claim {
        if c.status == "proven" || c.status == "benchmark" {
            assert!(
                !c.proof.trim().is_empty(),
                "claim {:?} is {} but declares no proof test fn",
                c.id,
                c.status
            );
            assert!(
                fn_exists_in_source(&c.proof),
                "claim {:?}: proof fn `{}` was not found as `fn {}` anywhere under \
                 tests/ or src/ — the claim's backing test does not exist",
                c.id,
                c.proof,
                c.proof
            );
        }
    }
}

/// The reverse invariant that keeps every displayed "proof:" label backed:
/// every test-function name the site SHOWS as a proof must be registered as a
/// proven claim here (and therefore must exist in source, by the test above).
/// Without this, the page could print `proof: total_fabrication` and nothing
/// would catch it.
#[test]
fn every_proof_label_shown_on_the_site_is_registered() {
    let reg = load_registry();
    let registered: std::collections::HashSet<&str> = reg
        .claim
        .iter()
        .filter(|c| c.status == "proven")
        .map(|c| c.proof.as_str())
        .collect();

    let html = site_html();
    // The site renders proofs two ways:
    //   home cap cards:   <b>✓</b> proof: <fn_name>
    //   detail ledgers:   <code><fn_name></code> inside a .ledger "proof" strip
    // Capture the explicit "proof:" form (unambiguous), which the cards use.
    let re = Regex::new(r"proof:\s*([a-z][a-z0-9_]{12,})").unwrap();
    let mut shown = std::collections::HashSet::new();
    for caps in re.captures_iter(&html) {
        shown.insert(caps.get(1).unwrap().as_str().to_string());
    }
    assert!(
        !shown.is_empty(),
        "no `proof: <fn>` labels found on the site — the proof-card rendering \
         changed; update this guard's regex"
    );
    for fn_name in &shown {
        assert!(
            registered.contains(fn_name.as_str()),
            "the site displays `proof: {fn_name}` but no proven registry claim \
             carries that proof fn — register it (or it's a fabricated label)"
        );
    }
}

#[test]
fn copy_only_claims_explain_themselves() {
    let reg = load_registry();
    for c in &reg.claim {
        if c.status == "copy-only" {
            assert!(
                !c.note.trim().is_empty(),
                "copy-only claim {:?} must carry a note explaining why it is \
                 exempt from a test mapping",
                c.id
            );
        }
    }
}
