//! Wave 9 — Marketing artifacts acceptance criteria.

#[test]
#[ignore = "wave 9 not yet shipped: 9.1 web/demo.cast asciinema demo file exists for landing page hero"]
fn demo_cast_exists() {
    assert!(
        std::path::Path::new("web/demo.cast").exists(),
        "missing web/demo.cast — create it during Wave 9.1"
    );
}

#[test]
#[ignore = "wave 9 not yet shipped: 9.1 web/demo.svg rendered asciinema SVG exists"]
fn demo_svg_exists() {
    assert!(
        std::path::Path::new("web/demo.svg").exists(),
        "missing web/demo.svg — create it during Wave 9.1"
    );
}

#[test]
#[ignore = "wave 9 not yet shipped: 9.2 README.md exists at repo root covering install, mental model, MCP, honest status, comparison table"]
fn readme_exists() {
    assert!(
        std::path::Path::new("README.md").exists(),
        "missing README.md — create it during Wave 9.2"
    );
}

#[test]
#[ignore = "wave 9 not yet shipped: 9.3 examples/riz.dev.toml uses AWS handler syntax handler = file.export"]
fn examples_use_aws_handler_syntax() {
    // Implementer verifies examples/riz.dev.toml uses "index.handler" style during Wave 9.3.
}

#[test]
#[ignore = "wave 9 not yet shipped: 9.4 landing page hero subhead uses corrected HTTP-specific microcopy"]
fn landing_page_hero_microcopy_updated() {
    // Implementer verifies web/index.html hero subhead updated during Wave 9.4.
}
