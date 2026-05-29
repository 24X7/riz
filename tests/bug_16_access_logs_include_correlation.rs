//! BUG-16 regression: access log lines must include both `request_id` and
//! `source_ip` so operators can correlate a customer report to a specific
//! request in the log stream.
//!
//! Asserted via structural lookup against `src/server.rs` (cheap, no
//! runtime cost). The dispatch hot path's three `push_log` call sites
//! (cache-hit, post-dispatch success, error fallback) must ALL emit
//! `req=...` and `ip=...` in their format strings. Behavioral
//! coverage of the same log fields is also exercised through
//! `tests/integration_test.rs` via the live log channel.

#[test]
fn server_access_logs_emit_request_id_and_source_ip() {
    let src = std::fs::read_to_string("src/server.rs").expect("read src/server.rs");

    // Scope the search to the request handler region — between the
    // function definition and the next `pub fn`. Cheap heuristic that
    // catches accidental future regressions where someone adds a new
    // push_log without correlation fields.
    let lines: Vec<&str> = src.lines().collect();
    let mut push_log_sites: Vec<(usize, String)> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().starts_with("state.push_log(") {
            // The format!(...) string usually lives 2-4 lines below.
            // Capture a 5-line window for the assertion message.
            let end = (idx + 5).min(lines.len());
            push_log_sites.push((idx + 1, lines[idx..end].join("\n")));
        }
    }
    assert!(
        push_log_sites.len() >= 3,
        "expected at least 3 push_log call sites in src/server.rs; found {}",
        push_log_sites.len()
    );

    for (line_num, snippet) in &push_log_sites {
        assert!(
            snippet.contains("req=") && snippet.contains("ip="),
            "BUG-16 regression: src/server.rs:{line_num} push_log site is missing \
             req= or ip= correlation fields. Snippet:\n{snippet}\n\n\
             Every access log must include both `req={{request_id}}` and \
             `ip={{source_ip}}` so failures can be correlated to specific requests."
        );
    }
}
