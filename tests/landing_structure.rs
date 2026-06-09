//! Structural contract for the landing page (`web/index.html`).
//!
//! These are byte-offset ordering assertions over the raw HTML — no external
//! crates, std only. They lock in the structural decisions made in Phase 0:
//!
//!   1. The Features column LEADS with "Agent & AI Integration" (first bucket),
//!      then "LLM Gateway" (second), ahead of every other feature bucket.
//!   2. An in-page `#compare` section exists, sitting after `#author` and
//!      before the footer (the old standalone `/vs` page folded inline).
//!   3. The nav no longer links to `/vs` (it points at `#compare`).
//!   4. Every "coming soon" roadmap bucket carries a stable `data-claim=` id.
//!
//! Marketing copy correctness stays a human/PR concern; this only guards
//! structure that later phases (claims registry) depend on.

use std::fs;
use std::path::PathBuf;

fn landing_html() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/index.html");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

/// First byte offset of `needle`, or panic with a helpful message.
fn offset_of(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("expected to find {needle:?} in web/index.html"))
}

#[test]
fn features_lead_with_agent_then_gateway() {
    let html = landing_html();

    // These are the <h4> feature headings, as they appear in the Features
    // column. (The `&amp;` is the literal HTML-encoded ampersand.)
    let agent = offset_of(&html, "<h4>Agent &amp; AI Integration</h4>");
    let gateway = offset_of(&html, "<h4>LLM Gateway</h4>");

    assert!(
        agent < gateway,
        "Agent & AI Integration must appear before LLM Gateway \
         (agent@{agent}, gateway@{gateway})"
    );
}

#[test]
fn agent_and_gateway_lead_all_other_feature_buckets() {
    let html = landing_html();

    let agent = offset_of(&html, "<h4>Agent &amp; AI Integration</h4>");
    let gateway = offset_of(&html, "<h4>LLM Gateway</h4>");

    // A representative sample of the *other* feature buckets. If Agent & AI is
    // truly the first bucket, it precedes all of these.
    for other in [
        "<h4>Function Runtimes</h4>",
        "<h4>Protocol Surface</h4>",
        "<h4>Configuration &amp; Routing</h4>",
        "<h4>Deployment &amp; Lifecycle</h4>",
        "<h4>Observability</h4>",
        "<h4>Developer CLI</h4>",
    ] {
        let o = offset_of(&html, other);
        assert!(
            agent < o,
            "Agent & AI Integration (@{agent}) must precede {other} (@{o})"
        );
        assert!(
            gateway < o,
            "LLM Gateway (@{gateway}) must precede {other} (@{o})"
        );
    }
}

#[test]
fn compare_section_sits_between_author_and_footer() {
    let html = landing_html();

    let author = offset_of(&html, "id=\"author\"");
    let compare = offset_of(&html, "id=\"compare\"");
    let footer = offset_of(&html, "<footer>");

    assert!(
        author < compare,
        "#compare (@{compare}) must come after #author (@{author})"
    );
    assert!(
        compare < footer,
        "#compare (@{compare}) must come before the footer (@{footer})"
    );
}

#[test]
fn nav_no_longer_links_to_standalone_vs_page() {
    let html = landing_html();
    assert!(
        !html.contains("href=\"/vs\""),
        "nav must not link to the standalone /vs page — it should point at #compare"
    );
}

#[test]
fn coming_soon_buckets_each_carry_a_data_claim_id() {
    let html = landing_html();

    let total_coming_soon = html.matches("class=\"bucket coming-soon\"").count();
    assert!(
        total_coming_soon > 0,
        "expected at least one bucket with class=\"bucket coming-soon\""
    );

    // Split on the <article tag and inspect each chunk that opens a coming-soon
    // bucket — it must also declare a data-claim attribute. We match the class
    // attribute itself (not the bare substring) so the `.coming-soon` CSS rule
    // up in <style> doesn't get mistaken for a bucket.
    let mut checked = 0usize;
    for chunk in html.split("<article") {
        if chunk.contains("class=\"bucket coming-soon\"") {
            checked += 1;
            assert!(
                chunk.contains("data-claim="),
                "a coming-soon bucket is missing its data-claim= id; chunk starts: {:?}",
                &chunk[..chunk.len().min(120)]
            );
        }
    }

    assert_eq!(
        checked, total_coming_soon,
        "every coming-soon occurrence should map to exactly one <article chunk"
    );
}
