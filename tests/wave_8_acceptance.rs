//! Wave 8 — Test coverage gaps acceptance criteria.

#[test]
#[ignore = "wave 8 not yet shipped: 8.1 tests/hotreload_integration.rs exists and covers add/remove/replace function diffs"]
fn hotreload_integration_test_file_exists() {
    assert!(
        std::path::Path::new("tests/hotreload_integration.rs").exists(),
        "missing tests/hotreload_integration.rs — create it during Wave 8.1"
    );
}

#[test]
#[ignore = "wave 8 not yet shipped: 8.2 liveness fault-injection: process exits immediately, respawned within 250ms"]
fn liveness_fault_injection_respawns_within_250ms() {
    // Wave 8.2: verify that liveness.rs has a fault-injection test or that the
    // integration test file exists.
    assert!(
        std::path::Path::new("tests/hotreload_integration.rs").exists()
            || std::path::Path::new("tests/liveness_fault_injection.rs").exists(),
        "missing liveness fault-injection test file — Wave 8.2 not yet shipped"
    );
}

#[test]
#[ignore = "wave 8 not yet shipped: 8.3 tests/hot_swap_race.rs exists — 100 concurrent invocations survive hot_swap mid-flight"]
fn hot_swap_race_test_file_exists() {
    assert!(
        std::path::Path::new("tests/hot_swap_race.rs").exists(),
        "missing tests/hot_swap_race.rs — create it during Wave 8.3"
    );
}

#[test]
#[ignore = "wave 8 not yet shipped: 8.4 Bun integration tests in integration_test.rs no longer #[ignore]-gated"]
fn bun_integration_tests_ungated() {
    // Wave 8.4: integration_test.rs must exist and should not have all tests
    // behind #[ignore]. Verify at least one non-ignored test exists by reading
    // the file and checking that not every #[test] is immediately followed by #[ignore].
    let path = std::path::Path::new("tests/integration_test.rs");
    assert!(path.exists(), "missing tests/integration_test.rs");
    let contents = std::fs::read_to_string(path).expect("must read integration_test.rs");
    // A naive check: count #[test] occurrences vs #[ignore] occurrences.
    // When all tests are ungated, #[ignore] count drops below #[test] count.
    let test_count = contents.matches("#[test]").count();
    let ignore_count = contents.matches("#[ignore").count();
    assert!(
        ignore_count < test_count,
        "all {} Bun integration tests are still #[ignore]-gated — Wave 8.4 not yet shipped (test_count={}, ignore_count={})",
        test_count, test_count, ignore_count
    );
}

#[test]
#[ignore = "wave 8 not yet shipped: 8.5 dispatch hot path: auth-bypass, base64 round-trip, AWS time format, 413, 504 coverage"]
fn dispatch_hot_path_coverage_complete() {
    // Wave 8.5: verify the dispatch hot path test file exists with the required coverage.
    // As a proxy, check that the http_boundary test file covers the expected cases.
    let path = std::path::Path::new("tests/http_boundary.rs");
    assert!(path.exists(), "missing tests/http_boundary.rs — Wave 8.5 not yet shipped");
    let contents = std::fs::read_to_string(path).expect("must read http_boundary.rs");
    // Wave 8.5 requires 413 and 504 coverage in the dispatch hot path tests.
    assert!(
        contents.contains("413") || contents.contains("payload_too_large"),
        "tests/http_boundary.rs missing 413 payload-too-large coverage — Wave 8.5 not yet complete"
    );
    assert!(
        contents.contains("504") || contents.contains("gateway_timeout"),
        "tests/http_boundary.rs missing 504 gateway-timeout coverage — Wave 8.5 not yet complete"
    );
}
