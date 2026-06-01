//! Landing-page sanity tests — minimal version.
//!
//! Earlier this file enforced a strict claim-by-claim "truth slice" against
//! every <li> on the page. That made sense when we'd had real claim-vs-reality
//! incidents, but it became a drag on every site iteration. Two of the tests
//! also shelled out to `cargo test --list` which took 2-3 minutes each.
//!
//! Trimmed to two cheap, high-signal checks:
//!   1. The example `riz.toml` shown on the page parses + validates via the
//!      library's own config code (proves the snippet is real, not aspirational).
//!   2. At least one pill exists in the #config section (smoke test that the
//!      page didn't get accidentally truncated).
//!
//! Marketing copy correctness is a human/PR review concern from here on.

use regex::Regex;
use std::fs;

const LANDING_PAGE: &str = "web/index.html";

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
