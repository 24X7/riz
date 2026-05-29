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
    fs::read_to_string(LANDING_PAGE).unwrap_or_else(|e| {
        panic!(
            "could not read {LANDING_PAGE}: {e} \
            — the landing page is part of the repo and missing it is a build break"
        )
    })
}

/// Extracts the inner text of the first `<pre>` block inside the section
/// with id="config". Strips HTML tags so the result is parseable TOML.
fn extract_config_toml_block(html: &str) -> String {
    let re = Regex::new(r#"(?s)<section[^>]*id="config".*?<pre>(.*?)</pre>"#).unwrap();
    let caps = re
        .captures(html)
        .expect("could not locate the #config <pre> block");
    strip_html_tags(&caps[1])
}

/// Extracts every `<span class="pill">…</span>` inside the section with
/// id="config". Handles multiple .pills groups (e.g. runtimes + protocols).
fn extract_pills(html: &str) -> Vec<String> {
    // Grab the entire #config section
    let re_section = Regex::new(r#"(?s)<section[^>]*id="config".*?</section>"#).unwrap();
    let section = re_section
        .find(html)
        .expect("could not locate the #config section")
        .as_str()
        .to_string();
    // Within it, find all <span class="pill"> — note: NOT pill-group-label
    let re_pill = Regex::new(r#"<span class="pill">(.*?)</span>"#).unwrap();
    re_pill
        .captures_iter(&section)
        .map(|c| c[1].trim().to_string())
        .collect()
}

/// Extracts every `<li>` text inside `#status .status-col` under the
/// heading that matches `heading_contains`.
fn extract_status_lis(html: &str, heading_contains: &str) -> Vec<String> {
    let re_section = Regex::new(r#"(?s)<section[^>]*id="status".*?</section>"#).unwrap();
    let status_block = re_section
        .find(html)
        .expect("could not locate the #status section")
        .as_str();
    let re_col = Regex::new(r#"(?s)<div class="status-col">\s*<h3>(.*?)</h3>(.*?)</div>"#).unwrap();
    let re_li = Regex::new(r#"<li>(.*?)</li>"#).unwrap();
    for caps in re_col.captures_iter(status_block) {
        if caps[1].contains(heading_contains) {
            return re_li
                .captures_iter(&caps[2])
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
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn extractors_smoke() {
    let h = html();
    let toml = extract_config_toml_block(&h);
    assert!(
        toml.contains("[function.api]"),
        "config block missing function header: {toml}"
    );
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
    cfg.validate()
        .unwrap_or_else(|e| panic!("landing-page riz.toml fails validation: {e}\n---\n{full}"));
}

/// Source-of-truth for every feature pill on the landing page.
/// Keep this in sync with the `.pills` blocks in `web/index.html` under #config.
/// Pills are grouped (runtimes + protocols) but the extractor collects all of them.
const PILLS: &[&str] = &[
    // runtimes group
    "bun",
    "python",
    "rust",
    // protocols group
    "http api v2",
    "websocket",
];

#[test]
fn pills_match_truth_slice() {
    let on_page: HashSet<String> = extract_pills(&html()).into_iter().collect();
    let in_code: HashSet<String> = PILLS.iter().map(|s| s.to_string()).collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(
        only_on_page.is_empty() && only_in_code.is_empty(),
        "Landing-page pills drift detected.\n\
         On page but not in PILLS truth slice: {only_on_page:?}\n\
         In PILLS but not on page: {only_in_code:?}\n\
         Fix one or the other."
    );
}

struct Claim {
    page_text: &'static str,
    proof: &'static str, // test fn name (Works now) or wave heading (Coming)
}

const WORKS_NOW: &[Claim] = &[
    Claim {
        page_text: "AWS API Gateway v2 HTTP payload — exact aws_lambda_events types",
        proof: "fixture_apigw_v2_http_simple_get_round_trips",
    },
    Claim {
        page_text: "Bun (TypeScript / JavaScript) handlers",
        proof: "runtime_registry_registers_bun",
    },
    Claim {
        page_text: "Python runtime adapter",
        proof: "python_runtime_accepted_by_config_validate",
    },
    Claim {
        page_text: "Rust runtime adapter",
        proof: "rust_runtime_accepted_by_config_validate",
    },
    Claim {
        page_text: "Function-centric config: [function.<name>] + N routes per function",
        proof: "function_centric_config_parses",
    },
    Claim {
        page_text: "AWS path syntax: {id}, {proxy+}, $default",
        proof: "router_matches_aws_path_syntax",
    },
    Claim {
        page_text: "AWS handler syntax: handler = \"index.handler\"",
        proof: "handler_export_syntax_resolves",
    },
    Claim {
        page_text: "Hot-swap deploys from S3 with in-flight request drain",
        proof: "hot_swap_drains_in_flight_requests",
    },
    Claim {
        page_text: "riz.toml hot-reload on save",
        proof: "hotreload_picks_up_riz_toml_changes",
    },
    Claim {
        page_text: "/_riz/health · /_riz/metrics · /_riz/registry",
        proof: "system_endpoints_respond_with_aws_shape",
    },
    Claim {
        page_text: "MCP server (spec 2024-11-05, JSON-RPC, batches, lifecycle)",
        proof: "mcp_spec_2024_11_05_lifecycle",
    },
    Claim {
        page_text: "Terminal dashboard: P50/P75/P90/P95/P99 over 5-min window",
        proof: "latency_window_emits_all_percentiles",
    },
    Claim {
        page_text: "Datadog metrics emitter",
        proof: "datadog_emitter_constructs_from_config",
    },
    Claim {
        page_text: "WebSocket APIs ($connect / $disconnect / $default) + @connections management API at /_riz/connections/{id}",
        proof: "websocket_echo_roundtrip",
    },
    Claim {
        page_text: "Lambda authorizers — REQUEST + JWT (JWKS, TTL cache)",
        proof: "request_authorizer_allows_valid_token",
    },
    Claim {
        page_text: "CORS auto-preflight — OPTIONS → 204, Access-Control-Allow-* headers",
        proof: "cors_preflight_returns_204_for_options",
    },
    Claim {
        page_text: "Bearer-token auth on /_riz/* (/_riz/health stays open)",
        proof: "riz_metrics_returns_401_without_auth_when_token_configured",
    },
    Claim {
        page_text: "Real Lambda context — getRemainingTimeInMillis, functionName, invokedFunctionArn, awsRequestId",
        proof: "context_remaining_time_uses_deadline",
    },
    Claim {
        page_text: "On-box safety profile — always-on RLIMIT_CORE=0 + RLIMIT_NOFILE=4096 + RLIMIT_FSIZE=100MiB",
        proof: "child_inherits_always_on_caps",
    },
    Claim {
        page_text: "Opt-in per-function caps — memory_mb (RLIMIT_AS), cpu_time_secs (RLIMIT_CPU), allowed_paths (Linux Landlock filesystem allowlist)",
        proof: "apply_per_function_limits_with_none_is_no_op",
    },
    Claim {
        page_text: "WebSocket connection list endpoint — GET /_riz/connections",
        proof: "ws_list_endpoint_includes_live_connection",
    },
];

const COMING: &[Claim] = &[Claim {
    page_text: "Non-HTTP event sources (SQS, SNS, S3, EventBridge) — v0.2",
    proof: "OutOfScope",
}];

fn normalize_li(s: &str) -> String {
    // Collapse internal whitespace so HTML formatting differences don't
    // false-fail the comparison.
    let s = html_decode_entities(s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn works_now_matches_truth_slice() {
    let on_page: HashSet<String> = extract_status_lis(&html(), "Works now")
        .into_iter()
        .map(|s| normalize_li(&s))
        .collect();
    let in_code: HashSet<String> = WORKS_NOW
        .iter()
        .map(|c| normalize_li(c.page_text))
        .collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(
        only_on_page.is_empty() && only_in_code.is_empty(),
        "Works-now drift detected.\n\
         On page but not in WORKS_NOW: {only_on_page:?}\n\
         In WORKS_NOW but not on page: {only_in_code:?}"
    );
}

#[test]
fn coming_matches_truth_slice() {
    let on_page: HashSet<String> = extract_status_lis(&html(), "Coming")
        .into_iter()
        .map(|s| normalize_li(&s))
        .collect();
    let in_code: HashSet<String> = COMING.iter().map(|c| normalize_li(c.page_text)).collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(
        only_on_page.is_empty() && only_in_code.is_empty(),
        "Coming drift detected.\n\
         On page but not in COMING: {only_on_page:?}\n\
         In COMING but not on page: {only_in_code:?}"
    );
}

#[test]
fn coming_proofs_reference_real_waves() {
    let roadmap =
        fs::read_to_string("docs/superpowers/plans/2026-05-26-v01-honest-ship-roadmap.md")
            .expect("roadmap missing");
    for claim in COMING {
        if claim.proof == "OutOfScope" {
            continue;
        }
        let needle = format!("## {}", claim.proof);
        let needle_alt = format!("## {} —", claim.proof);
        assert!(
            roadmap.contains(&needle) || roadmap.contains(&needle_alt),
            "COMING claim {:?} points at proof {:?}, but the roadmap has no matching heading",
            claim.page_text,
            claim.proof
        );
    }
}

#[test]
fn works_now_proof_tests_exist() {
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "test",
            "--workspace",
            "--all-targets",
            "--",
            "--list",
            "--format=terse",
        ])
        .output()
        .expect("failed to run `cargo test --list`");
    let listing = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<_> = WORKS_NOW
        .iter()
        .filter(|c| !listing.contains(c.proof))
        .map(|c| c.proof)
        .collect();
    assert!(
        missing.is_empty(),
        "WORKS_NOW claims point at proof tests that do not exist:\n  {missing:?}\n\
         Either rename the test, change the proof field, or write the missing test."
    );
}
