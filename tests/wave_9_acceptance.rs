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
    let path = std::path::Path::new("examples/riz.dev.toml");
    assert!(
        path.exists(),
        "missing examples/riz.dev.toml — Wave 9.3 not yet shipped"
    );
    let contents = std::fs::read_to_string(path).expect("must read examples/riz.dev.toml");
    // AWS handler syntax: "index.handler" — dot-separated file.export, not a file path.
    assert!(
        contents.contains("handler = \"index.handler\"")
            || contents.contains("handler = \"app.handler\"")
            || contents.contains(".handler\""),
        "examples/riz.dev.toml does not use AWS handler syntax (file.export) — Wave 9.3 not yet shipped"
    );
}

#[test]
fn landing_page_hero_microcopy_updated() {
    let path = std::path::Path::new("web/index.html");
    assert!(
        path.exists(),
        "missing web/index.html — Wave 9.4 not yet shipped"
    );
    let contents = std::fs::read_to_string(path).expect("must read web/index.html");
    // Wave 9.4: the hero subhead must not use the old generic tagline.
    // When shipped, the microcopy should mention HTTP endpoints or API routes.
    assert!(
        contents.contains("HTTP") || contents.contains("API") || contents.contains("endpoint"),
        "web/index.html hero subhead missing HTTP-specific microcopy — Wave 9.4 not yet shipped"
    );
}
