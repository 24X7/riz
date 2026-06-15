//! Wave 0.5 — Drift-prevention automation acceptance criteria.
//!
//! When the implementer subagent lands each acceptance behavior, it removes
//! the `#[ignore]` line from the matching test. The wave is "done" only when
//! every test in this file runs un-ignored.

#[test]
fn landing_page_contract_suite_runs() {
    // The drift-prevention acceptance: the site's structure + claims are
    // machine-guarded. The single-page landing_page_contract.rs / landing_
    // structure.rs were retired when the site went multi-page; their guards
    // now live in site_structure.rs and claims_truth.rs.
    assert!(std::path::Path::new("tests/site_structure.rs").exists());
    assert!(std::path::Path::new("tests/claims_truth.rs").exists());
}

#[test]
fn aws_contract_fixtures_exist() {
    for f in &[
        "tests/fixtures/aws/apigw_v2_http_simple_get.json",
        "tests/fixtures/aws/apigw_v2_http_post_with_body.json",
        "tests/fixtures/aws/apigw_v2_websocket_connect.json",
        "tests/fixtures/aws/apigw_v2_websocket_message.json",
        "tests/fixtures/aws/apigw_v2_websocket_disconnect.json",
    ] {
        assert!(std::path::Path::new(f).exists(), "missing fixture: {f}");
    }
}

#[test]
fn ci_workflow_exists() {
    assert!(std::path::Path::new(".github/workflows/ci.yml").exists());
}
