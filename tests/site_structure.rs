//! Structural contract for the riz site (`web/*.html`).
//!
//! Replaces the old single-page landing guards. The site is now multi-page
//! ("terminal acid" brand): home + agents / sandbox / gateway / compare /
//! start / docs / examples. These checks lock the load-bearing structure and
//! the truth/agent affordances the brand promises, without pinning the exact
//! marketing copy (that's the claims registry's job, via claims_truth.rs).

use std::fs;
use std::path::PathBuf;

fn site_pages() -> Vec<(PathBuf, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    let mut out = Vec::new();
    // Non-page HTML build assets (e.g. the og-image source card) are excluded —
    // they are not part of the navigable site.
    const NOT_A_PAGE: &[&str] = &["og-card.html"];
    for entry in fs::read_dir(&dir).expect("web/ exists") {
        let p = entry.unwrap().path();
        let is_page_html = p.extension().is_some_and(|e| e == "html")
            && !p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| NOT_A_PAGE.contains(&n));
        if is_page_html {
            let html = fs::read_to_string(&p).unwrap();
            out.push((p, html));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        out.len() >= 7,
        "expected the full multi-page site under web/"
    );
    out
}

fn page(name: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("web")
            .join(name),
    )
    .unwrap_or_else(|e| panic!("could not read web/{name}: {e}"))
}

// ─────────────────────────── core pages exist ───────────────────────────────

#[test]
fn the_expected_pages_are_present() {
    for name in [
        "index.html",
        "agents.html",
        "sandbox.html",
        "gateway.html",
        "compare.html",
        "start.html",
        "docs.html",
        "examples.html",
    ] {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("web")
            .join(name);
        assert!(p.exists(), "missing core page web/{name}");
    }
}

#[test]
fn every_page_shares_the_nav_and_stylesheet() {
    for (path, html) in site_pages() {
        // Prefix match, not exact: pages cache-bust the stylesheet with a
        // version query and carry a turbo-track attribute
        // (`href="site.css?v=…" data-turbo-track="reload"`), so pin the shared
        // `href="site.css` and let the query string / attributes vary.
        assert!(
            html.contains(r#"rel="stylesheet" href="site.css"#),
            "{} does not link the shared site.css",
            path.display()
        );
        assert!(
            html.contains(r#"class="mark" href="index.html"#),
            "{} is missing the brand nav mark",
            path.display()
        );
        for link in [
            "agents.html",
            "sandbox.html",
            "gateway.html",
            "compare.html",
            "start.html",
            "docs.html",
            "examples.html",
        ] {
            assert!(
                html.contains(&format!("href=\"{link}\"")),
                "{} nav is missing a link to {link}",
                path.display()
            );
        }
        // Turbo Drive must load on every page so nav is a same-origin SPA swap
        // (no full reload), and the vendored file must exist (self-hosted).
        assert!(
            html.contains(r#"<script src="turbo.min.js"></script>"#),
            "{} does not load turbo.min.js (Turbo Drive nav)",
            path.display()
        );
        // Every page must carry a social-share image so links unfurl with a card.
        assert!(
            html.contains(r#"property="og:image""#) && html.contains("og.png"),
            "{} is missing the og:image social-share tag",
            path.display()
        );
    }
    // The vendored runtime + generated/static deploy-root assets must exist.
    for asset in [
        "turbo.min.js",
        "og.png",
        "favicon.svg",
        "apple-touch-icon.png",
        "robots.txt",
        "sitemap.xml",
        "llms.txt",
        ".well-known/riz.json",
    ] {
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("web")
                .join(asset)
                .exists(),
            "web/{asset} (deploy-root asset) is missing"
        );
    }
}

#[test]
fn non_html_links_opt_out_of_turbo() {
    // .txt / .json are not HTML — Turbo must not try to swap them in. Every
    // same-origin non-HTML link carries data-turbo="false".
    for (path, html) in site_pages() {
        for (needle, _label) in [
            (r#"href="llms.txt""#, "llms.txt"),
            (r#"href=".well-known/riz.json""#, "riz.json"),
        ] {
            let mut from = 0;
            while let Some(i) = html[from..].find(needle) {
                let at = from + i;
                // the data-turbo="false" attribute sits right after the href
                let tail = &html[at..(at + needle.len() + 24).min(html.len())];
                assert!(
                    tail.contains(r#"data-turbo="false""#),
                    "{}: a {needle} link is missing data-turbo=\"false\" (Turbo would \
                     try to render the non-HTML resource)",
                    path.display()
                );
                from = at + needle.len();
            }
        }
    }
}

// ─────────────────────────── home: the six cap cards ────────────────────────

#[test]
fn home_leads_with_the_six_capability_cards() {
    let html = page("index.html");
    let cards = html.matches(r#"class="cap""#).count();
    assert!(
        cards >= 6,
        "home page must present at least six capability cards, found {cards}"
    );
    let mcp = html
        .find("Agent-native, zero glue")
        .expect("home missing the agent-native cap card");
    let gateway = html
        .find("LLM gateway, built in")
        .expect("home missing the LLM gateway cap card");
    assert!(mcp < gateway, "the agent card must lead the gateway card");
}

#[test]
fn home_carries_the_claims_as_code_ledger() {
    let html = page("index.html");
    assert!(
        html.contains("claims-as-code"),
        "home page is missing the claims-as-code ledger — the brand's truth promise"
    );
}

// ─────────────────────────── compare: the two contrasts ─────────────────────

#[test]
fn compare_contrasts_lambda_and_frameworks() {
    let html = page("compare.html");
    assert!(
        html.contains("AWS Lambda"),
        "compare page must contrast against AWS Lambda directly"
    );
    assert!(
        ["Express", "FastAPI", "Axum", "Hono"]
            .iter()
            .any(|f| html.contains(f)),
        "compare page must contrast against an established web framework"
    );
    assert!(
        html.contains("What's actually different") && html.contains("The verdict"),
        "compare page must keep its differences row (vs Lambda) and verdict row (vs frameworks)"
    );
    // riz scales by running as a container on a platform that autoscales it —
    // the compare page must explain that scale story (not imply it can't scale).
    assert!(
        html.contains("Cloud Run") && html.contains("Fargate") && html.contains("Container Apps"),
        "compare page must show how riz scales on container platforms \
         (Cloud Run / ECS+Fargate / Azure Container Apps)"
    );
}

// ─────────────────────────── dual audience: agents + humans ─────────────────

#[test]
fn every_page_is_agent_addressable() {
    for (path, html) in site_pages() {
        assert!(
            html.contains("/llms.txt"),
            "{} lost its agent affordance (llms.txt link)",
            path.display()
        );
        assert!(
            html.contains("FOR AGENTS"),
            "{} is missing the FOR AGENTS footer block",
            path.display()
        );
    }
}

// ─────────────────────────── retired fictions ───────────────────────────────

/// Strings that were retired as fictions on the old site and must never
/// reappear (RLIMIT contains "MIT", so we ban realistic mis-license phrasings).
const BANNED: &[&str] = &[
    "ctx.invokeModel",
    "semantic cache</b>",
    "MIT license",
    "MIT License",
    "License: MIT",
    "MIT-licensed",
];

#[test]
fn no_page_carries_a_retired_fiction() {
    for (path, html) in site_pages() {
        for banned in BANNED {
            assert!(
                !html.contains(banned),
                "{} contains retired fiction {banned:?}",
                path.display()
            );
        }
    }
}

// ─────────────────────────── embedded riz.toml is real ──────────────────────

#[test]
fn embedded_toml_snippets_parse_and_validate() {
    let mut checked = 0;
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();
    for (path, html) in site_pages() {
        let mut rest = html.as_str();
        while let Some(start) = rest.find("<pre data-riz-toml>") {
            let after = &rest[start + "<pre data-riz-toml>".len()..];
            let end = after.find("</pre>").expect("unclosed <pre data-riz-toml>");
            let raw = &after[..end];
            let text = raw
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&");
            let text = tag_re.replace_all(&text, "").to_string();
            let cfg: riz::config::Config = toml::from_str(&text).unwrap_or_else(|e| {
                panic!(
                    "{}: embedded riz.toml does not parse: {e}\n{text}",
                    path.display()
                )
            });
            cfg.validate().unwrap_or_else(|e| {
                panic!(
                    "{}: embedded riz.toml fails validation: {e}",
                    path.display()
                )
            });
            checked += 1;
            rest = &after[end..];
        }
    }
    assert!(
        checked >= 3,
        "expected several CI-validated riz.toml snippets on the site, found {checked}"
    );
}

// ─────────────────────────── test-count floor matches registry ──────────────

#[test]
fn test_count_floor_matches_registry() {
    let registry = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/claims/registry.toml"),
    )
    .unwrap();
    let html = page("index.html");
    if html.contains("900+") {
        assert!(
            registry.contains("page_text = \"900+\""),
            "the site states a 900+ test floor but the registry test-count claim doesn't match"
        );
    }
}
