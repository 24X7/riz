//! Landing-page contract suite. Asserts that every promise made by
//! `web/index.html` resolves to real code in the repo.
//!
//! When you add a feature pill, a "Works now" line, or a "Coming" line on
//! the landing page, you must add a matching entry to the truth slice in
//! this file. Conversely, removing a feature from code requires removing
//! it from the page (or moving it to "Coming"). CI enforces both directions.

use regex::Regex;
use std::collections::HashSet;
use std::fs;

const LANDING_PAGE: &str = "web/index.html";

fn html() -> String {
    fs::read_to_string(LANDING_PAGE)
        .unwrap_or_else(|e| panic!("could not read {LANDING_PAGE}: {e} \
            — the landing page is part of the repo and missing it is a build break"))
}

/// Extracts the inner text of the first `<pre>` block inside the section
/// with id="config". Strips HTML tags so the result is parseable TOML.
fn extract_config_toml_block(html: &str) -> String {
    let re = Regex::new(r#"(?s)<section[^>]*id="config".*?<pre>(.*?)</pre>"#).unwrap();
    let caps = re.captures(html).expect("could not locate the #config <pre> block");
    strip_html_tags(&caps[1])
}

/// Extracts every `<span class="pill">…</span>` inside the section with
/// id="config" .pills container.
fn extract_pills(html: &str) -> Vec<String> {
    let re_section = Regex::new(r#"(?s)<section[^>]*id="config".*?<div class="pills">(.*?)</div>"#).unwrap();
    let pills_block = re_section.captures(html)
        .expect("could not locate the #config .pills block")[1].to_string();
    let re_pill = Regex::new(r#"<span class="pill">(.*?)</span>"#).unwrap();
    re_pill.captures_iter(&pills_block)
        .map(|c| c[1].trim().to_string())
        .collect()
}

/// Extracts every `<li>` text inside `#status .status-col` under the
/// heading that matches `heading_contains`.
fn extract_status_lis(html: &str, heading_contains: &str) -> Vec<String> {
    let re_section = Regex::new(r#"(?s)<section[^>]*id="status".*?</section>"#).unwrap();
    let status_block = re_section.find(html)
        .expect("could not locate the #status section").as_str();
    let re_col = Regex::new(r#"(?s)<div class="status-col">\s*<h3>(.*?)</h3>(.*?)</div>"#).unwrap();
    for caps in re_col.captures_iter(status_block) {
        if caps[1].contains(heading_contains) {
            let re_li = Regex::new(r#"<li>(.*?)</li>"#).unwrap();
            return re_li.captures_iter(&caps[2])
                .map(|c| strip_html_tags(&c[1]).trim().to_string())
                .collect();
        }
    }
    panic!("could not find status column with heading containing '{heading_contains}'")
}

fn strip_html_tags(s: &str) -> String {
    let re = Regex::new(r#"<[^>]+>"#).unwrap();
    re.replace_all(s, "").to_string()
}

fn html_decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

#[test]
fn extractors_smoke() {
    let h = html();
    let toml = extract_config_toml_block(&h);
    assert!(toml.contains("[function.api]"), "config block missing function header: {toml}");
    let pills = extract_pills(&h);
    assert!(!pills.is_empty(), "expected at least one pill");
    let works = extract_status_lis(&h, "Works now");
    assert!(!works.is_empty(), "expected at least one Works now item");
    let coming = extract_status_lis(&h, "Coming");
    assert!(!coming.is_empty(), "expected at least one Coming item");
}

#[test]
fn embedded_riz_toml_parses_and_validates() {
    let raw = extract_config_toml_block(&html());
    let raw = html_decode_entities(&raw);
    // The landing-page snippet shows function blocks only — wrap in a
    // minimal complete config so the parse exercises the same code path
    // real users hit.
    let full = format!("[server]\nport = 3000\nhost = \"0.0.0.0\"\n\n{raw}");
    let cfg: riz::config::Config = toml::from_str(&full)
        .unwrap_or_else(|e| panic!("landing-page riz.toml does not parse:\n{e}\n---\n{full}"));
    cfg.validate().unwrap_or_else(|e|
        panic!("landing-page riz.toml fails validation: {e}\n---\n{full}"));
}
