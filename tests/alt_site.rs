//! Guards for the alternate site (web/alt/*). Lighter than the main landing
//! guards, but the same discipline: the alt site may not resurrect retired
//! fictions, must keep its agent affordances, and any riz.toml it shows must
//! actually validate.

use std::fs;
use std::path::PathBuf;

fn alt_pages() -> Vec<(PathBuf, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/alt");
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("web/alt exists") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "html") {
            let html = fs::read_to_string(&p).unwrap();
            out.push((p, html));
        }
    }
    assert!(!out.is_empty(), "web/alt has no pages");
    out
}

/// Strings that were retired as fictions on the main site and must never
/// reappear on the alt site either.
const BANNED: &[&str] = &[
    "ctx.invokeModel",         // API that never existed
    "semantic cache</b>",      // unshipped feature presented as live
    "MIT",                     // riz is Apache-2.0
];

#[test]
fn alt_pages_carry_no_retired_fictions() {
    for (path, html) in alt_pages() {
        for banned in BANNED {
            assert!(
                !html.contains(banned),
                "{} contains retired fiction {banned:?}",
                path.display()
            );
        }
    }
}

#[test]
fn every_alt_page_is_agent_addressable() {
    // The dual-audience promise: every page points agents at the machine
    // surface (llms.txt at minimum).
    for (path, html) in alt_pages() {
        assert!(
            html.contains("/llms.txt"),
            "{} lost its agent affordance (llms.txt link)",
            path.display()
        );
    }
}

#[test]
fn alt_test_count_floor_matches_registry() {
    // The alt site states the same honest floor the registry pins.
    let registry = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/claims/registry.toml"),
    )
    .unwrap();
    for (path, html) in alt_pages() {
        if html.contains("+</b> tests") || html.contains("900+") || html.contains("800+") {
            assert!(
                html.contains("900+") && registry.contains("900+ tests"),
                "{} states a test-count floor that doesn't match the registry",
                path.display()
            );
        }
    }
}

#[test]
fn alt_toml_snippets_validate() {
    // Any fenced riz.toml shown inside a <pre data-riz-toml> block must parse
    // and validate with the real config code. Pages opt in via the attribute.
    for (path, html) in alt_pages() {
        let mut rest = html.as_str();
        while let Some(start) = rest.find("<pre data-riz-toml>") {
            let after = &rest[start + "<pre data-riz-toml>".len()..];
            let end = after.find("</pre>").expect("unclosed pre");
            let raw = &after[..end];
            let text = raw
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&");
            // strip span tags
            let text = regex::Regex::new(r"<[^>]+>")
                .unwrap()
                .replace_all(&text, "")
                .to_string();
            let cfg: riz::config::Config = toml::from_str(&text).unwrap_or_else(|e| {
                panic!("{}: embedded riz.toml does not parse: {e}\n{text}", path.display())
            });
            cfg.validate().unwrap_or_else(|e| {
                panic!("{}: embedded riz.toml fails validation: {e}", path.display())
            });
            rest = &after[end..];
        }
    }
}
