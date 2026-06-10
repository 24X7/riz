//! Claims-truth enforcement — the machine check that holds the homepage's
//! capability claims to reality.
//!
//! `tests/claims/registry.toml` maps each headline capability claim on the
//! landing page (`web/index.html`) to one of four honest statuses:
//!
//!   * `proven`     — backed by a REAL test fn that exists in `tests/` or `src/`.
//!   * `benchmark`  — a perf number proved by a benches/ recipe, with a
//!                    deterministic CI-floor sibling test as its `proof`.
//!   * `coming-soon`— a roadmap bucket; must carry a `data-claim=` ribbon and a
//!                    `roadmap` pointer, and must NOT masquerade as a shipped
//!                    Features-column claim.
//!   * `copy-only`  — subjective/marketing or a point-in-time stat; needs a
//!                    `note` saying why it's exempt from a test mapping.
//!
//! This test enforces every one of those invariants so a claim can't silently
//! drift away from the code that's supposed to back it.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

const LANDING_PAGE: &str = "web/index.html";
const REGISTRY: &str = "tests/claims/registry.toml";

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
    data_claim: String,
    #[serde(default)]
    roadmap: String,
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

/// Tag-stripped rendered text of the landing page. Entities are left as-is
/// (`&amp;`, `&lt;`) — registry `page_text` values are written to match.
fn page_text(html: &str) -> String {
    strip_html_tags(html)
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

/// The Features (v0.1, "in production") column text: everything between the
/// Features `<h3>` and the Roadmap `<h3>`. A `coming-soon` claim's page_text
/// must NOT appear in here.
fn features_column(html: &str) -> String {
    let start = html
        .find("<h3>Features</h3>")
        .expect("could not find the Features <h3> on the page");
    let roadmap = html
        .find("<h3>Roadmap</h3>")
        .expect("could not find the Roadmap <h3> on the page");
    assert!(
        start < roadmap,
        "Features <h3> must come before Roadmap <h3>"
    );
    strip_html_tags(&html[start..roadmap])
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
        assert!(
            seen.insert(c.id.clone()),
            "duplicate claim id: {:?}",
            c.id
        );
        assert!(
            matches!(
                c.status.as_str(),
                "proven" | "benchmark" | "coming-soon" | "copy-only"
            ),
            "claim {:?} has unknown status {:?}",
            c.id,
            c.status
        );
    }
}

#[test]
fn every_claim_page_text_appears_on_the_page() {
    let reg = load_registry();
    let text = page_text(&read(LANDING_PAGE));
    for c in &reg.claim {
        assert!(
            !c.page_text.trim().is_empty(),
            "claim {:?} has an empty page_text drift guard",
            c.id
        );
        assert!(
            text.contains(&c.page_text),
            "claim {:?}: page_text {:?} does not appear in the rendered page text \
             — registry and page have drifted apart",
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

#[test]
fn coming_soon_claims_are_ribbons_not_shipped_claims() {
    let reg = load_registry();
    let html = read(LANDING_PAGE);
    let features = features_column(&html);

    for c in &reg.claim {
        if c.status != "coming-soon" {
            continue;
        }
        assert!(
            !c.data_claim.trim().is_empty(),
            "coming-soon claim {:?} must reference a data_claim id",
            c.id
        );
        assert!(
            !c.roadmap.trim().is_empty(),
            "coming-soon claim {:?} must carry a roadmap pointer",
            c.id
        );

        // The data-claim must live on an actual coming-soon bucket article.
        let mut found = false;
        for chunk in html.split("<article") {
            if chunk.contains("class=\"bucket coming-soon\"")
                && chunk.contains(&format!("data-claim=\"{}\"", c.data_claim))
            {
                found = true;
                break;
            }
        }
        assert!(
            found,
            "coming-soon claim {:?}: data-claim=\"{}\" not found inside any \
             <article class=\"bucket coming-soon\"> block",
            c.id,
            c.data_claim
        );

        // A roadmap item must NOT appear as a shipped Features-column claim.
        assert!(
            !features.contains(&c.page_text),
            "coming-soon claim {:?}: its page_text {:?} appears inside the \
             Features (v0.1, in-production) column — a roadmap item is \
             masquerading as shipped",
            c.id,
            c.page_text
        );
    }
}

#[test]
fn every_data_claim_ribbon_is_registered_as_coming_soon() {
    let reg = load_registry();
    let html = read(LANDING_PAGE);

    let re = Regex::new(r#"data-claim="([^"]+)""#).unwrap();
    for caps in re.captures_iter(&html) {
        let id = &caps[1];
        let matched = reg.claim.iter().find(|c| {
            (c.id == *id || c.data_claim == *id) && c.status == "coming-soon"
        });
        assert!(
            matched.is_some(),
            "the page carries data-claim=\"{id}\" but no coming-soon registry \
             claim maps to it — either register it or remove the ribbon \
             (no orphan ribbons; no unregistered not-yet-proven claim)"
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

// Keep the import used even if cfg gates change.
#[allow(dead_code)]
fn _manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
