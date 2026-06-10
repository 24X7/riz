//! Landing-page sanity tests.
//!
//! Two cheap, high-signal checks that prove the on-page `riz.toml` is real:
//!   1. The example `riz.toml` shown on the page parses + validates via the
//!      library's own config code (proves the snippet is real, not aspirational).
//!   2. At least one pill exists in the #config section (smoke test that the
//!      page didn't get accidentally truncated).
//!
//! Claim-vs-reality enforcement — every headline capability claim mapped to a
//! REAL backing test — is the job of `tests/claims_truth.rs` against
//! `tests/claims/registry.toml`. That is the authoritative truth check; this
//! file only adds one bridge test below (`every_page_claim_is_registered`) so
//! the Proof-bucket numeric claims (test count, bug-tracker line) on the page
//! can't silently drift out of the registry.

use regex::Regex;
use std::fs;

const LANDING_PAGE: &str = "web/index.html";
const REGISTRY: &str = "tests/claims/registry.toml";

fn html() -> String {
    fs::read_to_string(LANDING_PAGE).unwrap_or_else(|e| {
        panic!(
            "could not read {LANDING_PAGE}: {e} \
            — the landing page is part of the repo and missing it is a build break"
        )
    })
}

fn strip_html_tags(s: &str) -> String {
    let re = Regex::new(r#"<[^>]+>"#).unwrap();
    re.replace_all(s, "").to_string()
}

fn html_decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Pull the inner text of the first <pre> block inside the #config section.
fn extract_config_toml_block(html: &str) -> String {
    let re = Regex::new(r#"(?s)<section[^>]*id="config".*?<pre>(.*?)</pre>"#).unwrap();
    let caps = re
        .captures(html)
        .expect("could not locate the #config <pre> block");
    html_decode_entities(&strip_html_tags(&caps[1]))
}

#[test]
fn embedded_riz_toml_parses_and_validates() {
    let toml_text = extract_config_toml_block(&html());
    let config: riz::config::Config = toml::from_str(&toml_text).unwrap_or_else(|e| {
        panic!(
            "the riz.toml snippet on the landing page does not parse:\n{e}\n\n\
             Snippet:\n{toml_text}"
        )
    });
    config.validate().unwrap_or_else(|e| {
        panic!(
            "the riz.toml snippet on the landing page parses but fails validation:\n{e}\n\n\
             Snippet:\n{toml_text}"
        )
    });
}

#[test]
fn config_section_has_pills() {
    let h = html();
    let re_section = Regex::new(r#"(?s)<section[^>]*id="config".*?</section>"#).unwrap();
    let section = re_section
        .find(&h)
        .expect("could not locate the #config section")
        .as_str();
    let re_pill = Regex::new(r#"<span class="pill">[^<]*</span>"#).unwrap();
    let pill_count = re_pill.captures_iter(section).count();
    assert!(
        pill_count > 0,
        "expected at least one .pill inside #config section — page may be truncated"
    );
}

/// Bridge test: the claims registry is non-trivial, and the Proof-bucket
/// numeric claims that live on the page (the `cargo nextest run` test count and
/// the production-readiness bug-tracker line) are each reflected as a registry
/// claim. This stops a Proof-bucket number from drifting on the page without a
/// corresponding registry entry (which `claims_truth.rs` then holds to the page
/// text and an honest status).
#[test]
fn every_page_claim_is_registered() {
    let registry = fs::read_to_string(REGISTRY)
        .unwrap_or_else(|e| panic!("could not read {REGISTRY}: {e}"));
    let page = html();

    // Non-trivial registry: well more than a placeholder.
    let claim_count = registry.matches("[[claim]]").count();
    assert!(
        claim_count >= 10,
        "claims registry looks trivial ({claim_count} claims) — \
         every live capability claim should be mapped"
    );

    // The page's Proof bucket states a test count like "800+ tests" (an honest
    // floor, not a pinned number — see the test-count claim's note). Extract it
    // (optional trailing '+') and require that exact string to be registered.
    let test_count = Regex::new(r"(\d{2,5}\+?) tests \(<code>cargo nextest run</code>\)")
        .unwrap()
        .captures(&page)
        .map(|c| c[1].to_string())
        .expect("could not find the '<N> tests (cargo nextest run)' Proof-bucket line");
    let test_count_phrase = format!("{test_count} tests");
    assert!(
        registry.contains(&test_count_phrase),
        "the page advertises \"{test_count_phrase}\" but no registry claim carries \
         that page_text — update the test-count claim in {REGISTRY}"
    );

    // The bug-tracker Proof line must likewise be mirrored in the registry.
    let bug_line = "Production-readiness bug tracker closed, with regression-gate tests for the fixed bugs";
    assert!(
        page.contains(bug_line),
        "the bug-tracker Proof line changed on the page; update this test and \
         the bug-tracker-closed registry claim to match"
    );
    assert!(
        registry.contains(bug_line),
        "the bug-tracker Proof line on the page is not reflected in {REGISTRY} \
         — register it so it can't silently drift"
    );
}
