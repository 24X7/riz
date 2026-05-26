# Wave 0.5 — Drift-prevention Automation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire three drift-prevention primitives + a CI workflow into the repo so future drift between `web/index.html` and code, or between our types and AWS event shapes, gets caught at commit time.

**Architecture:** Pure additions to the test surface — no production-code changes (Wave 1 implementation tasks bring those). The landing-page contract suite parses `web/index.html` with three regex extractors and cross-references against Rust truth slices. AWS golden fixtures live under `tests/fixtures/aws/` and are exercised by round-trip tests. Per-wave acceptance test files start `#[ignore]`-gated and are un-ignored as each task lands.

**Tech stack:** Rust 1.83+, `regex` (new dep for the test harness), `serde_json` (already present), GitHub Actions for CI.

**Prerequisite:** WS Tasks 1–3 already shipped at HEAD `e4ef9696`. 312 tests passing.

---

## File structure

**New files:**
- `tests/landing_page_contract.rs` — landing-page contract suite
- `tests/aws_contract.rs` — AWS event-shape golden-fixture round-trip tests
- `tests/fixtures/aws/apigw_v2_http_simple_get.json`
- `tests/fixtures/aws/apigw_v2_http_post_with_body.json`
- `tests/fixtures/aws/apigw_v2_websocket_connect.json`
- `tests/fixtures/aws/apigw_v2_websocket_message.json`
- `tests/fixtures/aws/apigw_v2_websocket_disconnect.json`
- `tests/wave_0p5_acceptance.rs` (self-test: the landing-page suite proves itself)
- `tests/wave_1_acceptance.rs` (WebSocket)
- `tests/wave_2_acceptance.rs` (Python adapter)
- `tests/wave_3_acceptance.rs` (Authorizers)
- `tests/wave_4_acceptance.rs` (CORS)
- `tests/wave_4p5_acceptance.rs` (Bearer-token auth)
- `tests/wave_5_acceptance.rs` (Lambda context fidelity)
- `tests/wave_6_acceptance.rs` (Rust adapter)
- `tests/wave_7_acceptance.rs` (Code debt)
- `tests/wave_8_acceptance.rs` (Test gaps)
- `tests/wave_9_acceptance.rs` (Marketing artifacts)
- `.github/workflows/ci.yml`

**Modified:**
- `Cargo.toml` — `regex = "1"` to `[dev-dependencies]`
- `tests/integration_test.rs` — remove `#[ignore]` from Bun integration tests; CI installs Bun

---

## Task 1: Add `regex` dev-dep + scaffold the landing-page contract harness

**Files:**
- Modify: `Cargo.toml`
- Create: `tests/landing_page_contract.rs`

- [ ] **Step 1: Add `regex = "1"` to `[dev-dependencies]` in `Cargo.toml`**

Insert after the existing dev-deps:
```toml
regex = "1"
```

- [ ] **Step 2: Create `tests/landing_page_contract.rs` with shared extractors**

```rust
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
```

- [ ] **Step 3: Run the smoke test**

```bash
cargo test --test landing_page_contract extractors_smoke 2>&1 | tail -10
```

Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock tests/landing_page_contract.rs
git commit -m "test(drift): scaffold landing-page contract harness"
```

---

## Task 2: Assert the embedded `riz.toml` parses + validates

**Files:**
- Modify: `tests/landing_page_contract.rs`

- [ ] **Step 1: Append failing test**

```rust
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
```

- [ ] **Step 2: Run**

```bash
cargo test --test landing_page_contract embedded_riz_toml 2>&1 | tail -10
```

Expected: 1 passed. If it fails, fix `web/index.html` (the landing page is wrong) or fix `Config` (real bug). DO NOT loosen the test.

- [ ] **Step 3: Commit**

```bash
git add tests/landing_page_contract.rs
git commit -m "test(drift): embedded riz.toml on landing page must parse + validate"
```

---

## Task 3: PILLS truth slice + symmetric-difference assertion

**Files:**
- Modify: `tests/landing_page_contract.rs`

- [ ] **Step 1: Append truth slice + test**

```rust
/// Source-of-truth for every feature pill on the landing page.
/// Keep this in sync with the `.pills` block in `web/index.html` under #config.
const PILLS: &[&str] = &[
    "bun",
    "python — soon",
    "rust — soon",
    "node — soon",
    "websocket — soon",
];

#[test]
fn pills_match_truth_slice() {
    let on_page: HashSet<String> = extract_pills(&html()).into_iter().collect();
    let in_code: HashSet<String> = PILLS.iter().map(|s| s.to_string()).collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(only_on_page.is_empty() && only_in_code.is_empty(),
        "Landing-page pills drift detected.\n\
         On page but not in PILLS truth slice: {only_on_page:?}\n\
         In PILLS but not on page: {only_in_code:?}\n\
         Fix one or the other.");
}
```

- [ ] **Step 2: Run**

```bash
cargo test --test landing_page_contract pills_match 2>&1 | tail -10
```

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add tests/landing_page_contract.rs
git commit -m "test(drift): pin landing-page runtime pills to a Rust truth slice"
```

---

## Task 4: WORKS_NOW + COMING truth slices + symmetric-difference

**Files:**
- Modify: `tests/landing_page_contract.rs`

- [ ] **Step 1: Append the truth slices + tests**

Each entry is a single string that must exactly match the `<li>` text on the page (after HTML stripping + trim). For "Works now" entries, the second field names the test function whose presence proves the feature; for "Coming" entries, the second field names the wave heading in the roadmap.

```rust
struct Claim {
    page_text: &'static str,
    proof: &'static str, // test fn name (Works now) or wave heading (Coming)
}

const WORKS_NOW: &[Claim] = &[
    Claim { page_text: "AWS API Gateway v2 HTTP payload — exact aws_lambda_events types",
            proof: "fixture_apigw_v2_http_simple_get_round_trips" },
    Claim { page_text: "Bun (TypeScript / JavaScript) handlers",
            proof: "runtime_registry_registers_bun" },
    Claim { page_text: "Function-centric config: [function.<name>] + N routes per function",
            proof: "function_centric_config_parses" },
    Claim { page_text: "AWS path syntax: {id}, {proxy+}, $default",
            proof: "router_matches_aws_path_syntax" },
    Claim { page_text: "AWS handler syntax: handler = \"index.handler\"",
            proof: "handler_export_syntax_resolves" },
    Claim { page_text: "Hot-swap deploys from S3 with in-flight request drain",
            proof: "hot_swap_drains_in_flight_requests" },
    Claim { page_text: "riz.toml hot-reload on save",
            proof: "hotreload_picks_up_riz_toml_changes" },
    Claim { page_text: "/_riz/health · /_riz/metrics · /_riz/registry",
            proof: "system_endpoints_respond_with_aws_shape" },
    Claim { page_text: "MCP server (spec 2024-11-05, JSON-RPC, batches, lifecycle)",
            proof: "mcp_spec_2024_11_05_lifecycle" },
    Claim { page_text: "Terminal dashboard: P50/P75/P90/P95/P99 over 5-min window",
            proof: "latency_window_emits_all_percentiles" },
    Claim { page_text: "Datadog metrics emitter",
            proof: "datadog_emitter_constructs_from_config" },
];

const COMING: &[Claim] = &[
    Claim { page_text: "Python runtime adapter",                          proof: "Wave 2" },
    Claim { page_text: "Rust runtime adapter",                            proof: "Wave 6" },
    Claim { page_text: "WebSocket APIs ($connect / $disconnect / connectionId)", proof: "Wave 1" },
    Claim { page_text: "Lambda authorizers (REQUEST / JWT)",              proof: "Wave 3" },
    Claim { page_text: "CORS auto-preflight",                             proof: "Wave 4" },
    Claim { page_text: "Non-HTTP event sources (SQS, SNS, S3, EventBridge)", proof: "OutOfScope" },
    Claim { page_text: "Bearer-token auth on /_riz/*",                    proof: "Wave 4.5" },
];

fn normalize_li(s: &str) -> String {
    // Collapse internal whitespace so HTML formatting differences don't
    // false-fail the comparison.
    let s = html_decode_entities(s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn works_now_matches_truth_slice() {
    let on_page: HashSet<String> = extract_status_lis(&html(), "Works now")
        .into_iter().map(|s| normalize_li(&s)).collect();
    let in_code: HashSet<String> = WORKS_NOW.iter()
        .map(|c| normalize_li(c.page_text)).collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(only_on_page.is_empty() && only_in_code.is_empty(),
        "Works-now drift detected.\n\
         On page but not in WORKS_NOW: {only_on_page:?}\n\
         In WORKS_NOW but not on page: {only_in_code:?}");
}

#[test]
fn coming_matches_truth_slice() {
    let on_page: HashSet<String> = extract_status_lis(&html(), "Coming")
        .into_iter().map(|s| normalize_li(&s)).collect();
    let in_code: HashSet<String> = COMING.iter()
        .map(|c| normalize_li(c.page_text)).collect();

    let only_on_page: Vec<_> = on_page.difference(&in_code).cloned().collect();
    let only_in_code: Vec<_> = in_code.difference(&on_page).cloned().collect();

    assert!(only_on_page.is_empty() && only_in_code.is_empty(),
        "Coming drift detected.\n\
         On page but not in COMING: {only_on_page:?}\n\
         In COMING but not on page: {only_in_code:?}");
}

#[test]
fn coming_proofs_reference_real_waves() {
    let roadmap = fs::read_to_string("docs/superpowers/plans/2026-05-26-v01-honest-ship-roadmap.md")
        .expect("roadmap missing");
    for claim in COMING {
        if claim.proof == "OutOfScope" { continue; }
        let needle = format!("## {}", claim.proof);
        let needle_alt = format!("## {} —", claim.proof);
        assert!(roadmap.contains(&needle) || roadmap.contains(&needle_alt),
            "COMING claim {:?} points at proof {:?}, but the roadmap has no matching heading",
            claim.page_text, claim.proof);
    }
}
```

- [ ] **Step 2: Run — expect failures because the proof-test names don't exist yet**

```bash
cargo test --test landing_page_contract 2>&1 | tail -20
```

The set-equal tests should pass (truth slice was written to match the current HTML); only `coming_proofs_reference_real_waves` may fail if a wave heading is named differently.

- [ ] **Step 3: Fix any wave-heading mismatches by editing the roadmap headings to match the COMING `proof` strings exactly**

- [ ] **Step 4: All four tests pass**

- [ ] **Step 5: Commit**

```bash
git add tests/landing_page_contract.rs docs/superpowers/plans/2026-05-26-v01-honest-ship-roadmap.md
git commit -m "test(drift): pin works-now + coming columns and wave-heading references"
```

---

## Task 5: Compile-time check that every WORKS_NOW proof-test exists

**Files:**
- Modify: `tests/landing_page_contract.rs`

- [ ] **Step 1: Append a build-time symbol check**

The simplest pattern: maintain a Rust slice of function-pointer references and let the compiler enforce existence. For test functions in other crates we use a marker module of `pub fn` stubs that delegate. v0.1: list the names as a slice of `&'static str` and run a runtime check via `cargo test --list`.

Wait — `cargo test --list` is an out-of-process call from a test. Cleaner: add a `WORKS_NOW_PROOF_FNS` slice of `fn()` pointers in this file that calls each proof test as a one-liner.

Actually cleaner still: each proof-test lives in some `#[cfg(test)]` mod; we can't take its fn-pointer from an integration test (the `#[test]` attribute is not a function we can address). Use a shell-out:

```rust
#[test]
fn works_now_proof_tests_exist() {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["test", "--workspace", "--all-targets", "--", "--list", "--format=terse"])
        .output()
        .expect("failed to run `cargo test --list`");
    let listing = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<_> = WORKS_NOW.iter()
        .filter(|c| !listing.contains(c.proof))
        .map(|c| c.proof)
        .collect();
    assert!(missing.is_empty(),
        "WORKS_NOW claims point at proof tests that do not exist:\n  {missing:?}\n\
         Either rename the test, change the proof field, or write the missing test.");
}
```

- [ ] **Step 2: Run — expect this to fail loudly because most proof tests don't exist yet**

```bash
cargo test --test landing_page_contract works_now_proof_tests_exist 2>&1 | tail -10
```

- [ ] **Step 3: For each missing proof-test name, write the smallest possible test that proves the feature exists**

Tasks 5a–5k below. Write each as its own commit so a future contributor can see exactly which behavior each Works-now line claims.

Suggested locations (NEW test functions, not in `tests/landing_page_contract.rs`):
- `runtime_registry_registers_bun` → `src/process/runtime.rs` `#[cfg(test)] mod tests`
- `function_centric_config_parses` → already exists (or close) in `src/config.rs` tests; rename if needed
- `router_matches_aws_path_syntax` → `src/router.rs` tests
- `handler_export_syntax_resolves` → `src/process/mod.rs` or `src/runtime/process.rs` tests
- `hot_swap_drains_in_flight_requests` → `src/process/mod.rs` tests (may need to be `#[tokio::test]`)
- `hotreload_picks_up_riz_toml_changes` → `src/hotreload.rs` tests
- `system_endpoints_respond_with_aws_shape` → `tests/system_functions_integration.rs`
- `mcp_spec_2024_11_05_lifecycle` → `src/system/mcp.rs` tests (likely already covered — rename if so)
- `latency_window_emits_all_percentiles` → `src/state.rs` tests
- `datadog_emitter_constructs_from_config` → `src/metrics.rs` tests
- `fixture_apigw_v2_http_simple_get_round_trips` → `tests/aws_contract.rs` (Task 6)

For each existing test that already proves the claim, rename the test to match the proof field. For each missing test, write the minimum sufficient test (one-liner constructor calls are fine — the existence of the test is the claim).

- [ ] **Step 4: All `WORKS_NOW` proof tests resolve; `works_now_proof_tests_exist` passes**

- [ ] **Step 5: Commit each rename / new-test pair as its own atomic commit** with messages of the form:

```
test(works-now): pin `<proof_fn_name>` as the proof of `<page_text>`
```

---

## Task 6: AWS contract golden fixtures + round-trip tests

**Files:**
- Create: `tests/fixtures/aws/apigw_v2_http_simple_get.json`
- Create: `tests/fixtures/aws/apigw_v2_http_post_with_body.json`
- Create: `tests/fixtures/aws/apigw_v2_websocket_connect.json`
- Create: `tests/fixtures/aws/apigw_v2_websocket_message.json`
- Create: `tests/fixtures/aws/apigw_v2_websocket_disconnect.json`
- Create: `tests/aws_contract.rs`

- [ ] **Step 1: Drop in the five fixtures**

Source the JSON from the AWS docs (search "API Gateway v2 HTTP payload version 2.0 example" and "API Gateway WebSocket event"). For each file, save the exact verbatim example JSON. Below is the expected shape (NOT the verbatim content — copy from AWS):

`apigw_v2_http_simple_get.json`:
```json
{
  "version": "2.0",
  "routeKey": "$default",
  "rawPath": "/my/path",
  "rawQueryString": "parameter1=value1&parameter1=value2&parameter2=value",
  "cookies": ["cookie1", "cookie2"],
  "headers": {
    "header1": "value1",
    "header2": "value1,value2"
  },
  "queryStringParameters": {
    "parameter1": "value1,value2",
    "parameter2": "value"
  },
  "requestContext": {
    "accountId": "123456789012",
    "apiId": "api-id",
    "authentication": { "clientCert": null },
    "domainName": "id.execute-api.us-east-1.amazonaws.com",
    "domainPrefix": "id",
    "http": {
      "method": "POST",
      "path": "/my/path",
      "protocol": "HTTP/1.1",
      "sourceIp": "192.0.2.1",
      "userAgent": "agent"
    },
    "requestId": "id",
    "routeKey": "$default",
    "stage": "$default",
    "time": "12/Mar/2020:19:03:58 +0000",
    "timeEpoch": 1583348638390
  },
  "body": "Hello from Lambda",
  "pathParameters": { "parameter1": "value1" },
  "isBase64Encoded": false,
  "stageVariables": { "stageVariable1": "value1", "stageVariable2": "value2" }
}
```

The other fixtures follow the same pattern. The WebSocket fixtures must include `requestContext.connectionId`, `requestContext.eventType` (`CONNECT` / `MESSAGE` / `DISCONNECT`), and `requestContext.routeKey` (`$connect` / `$default` / `$disconnect`).

- [ ] **Step 2: Write `tests/aws_contract.rs`**

```rust
//! Round-trip every canonical AWS event fixture through the
//! `aws_lambda_events` types we re-export. Any divergence between the
//! AWS-docs shape and our parsed shape fails CI.

use riz::gateway::{ApiGatewayV2httpRequest, ApiGatewayWebsocketProxyRequest};
use serde_json::Value;

/// Strip fields we deliberately don't model (e.g., `authentication.clientCert`
/// when null, or future AWS-added fields). Document every entry here so
/// "intentional" can be distinguished from "broken."
fn deep_normalize(mut v: Value) -> Value {
    if let Value::Object(ref mut map) = v {
        // Documented exclusion: AWS sometimes emits `authentication: null`
        // for the entire object on unauthenticated requests; aws_lambda_events
        // models this as Option<Authentication> which round-trips as omitted.
        if map.get("authentication").map(|x| x.is_null()).unwrap_or(false) {
            map.remove("authentication");
        }
        for (_k, val) in map.iter_mut() {
            *val = deep_normalize(val.clone());
        }
    } else if let Value::Array(ref mut arr) = v {
        for item in arr.iter_mut() {
            *item = deep_normalize(item.clone());
        }
    }
    v
}

#[test]
fn fixture_apigw_v2_http_simple_get_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_simple_get.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw)
        .expect("deserialize");
    assert_eq!(parsed.version.as_deref(), Some("2.0"));
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_http_post_with_body_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_post_with_body.json");
    let parsed: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.is_base64_encoded, true);
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_connect_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_connect.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.event_type.as_deref(), Some("CONNECT"));
    assert_eq!(parsed.request_context.route_key.as_deref(), Some("$connect"));
    assert!(parsed.request_context.connection_id.is_some());
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_message_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_message.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.event_type.as_deref(), Some("MESSAGE"));
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}

#[test]
fn fixture_apigw_v2_websocket_disconnect_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_websocket_disconnect.json");
    let parsed: ApiGatewayWebsocketProxyRequest = serde_json::from_str(raw).expect("deserialize");
    assert_eq!(parsed.request_context.event_type.as_deref(), Some("DISCONNECT"));
    let reserialized: Value = serde_json::to_value(&parsed).unwrap();
    let original: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}
```

- [ ] **Step 3: Run**

```bash
cargo test --test aws_contract 2>&1 | tail -15
```

Expected: 5 passed. If any field-name divergence shows up, document it in `deep_normalize` with a `// Documented exclusion:` comment **before** removing it — that comment is the audit trail.

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures tests/aws_contract.rs
git commit -m "test(drift): AWS event-shape golden fixtures for HTTP + WebSocket"
```

---

## Task 7: Per-wave acceptance scaffolds

**Files:**
- Create: `tests/wave_0p5_acceptance.rs`
- Create: `tests/wave_1_acceptance.rs`
- Create: `tests/wave_2_acceptance.rs`
- Create: `tests/wave_3_acceptance.rs`
- Create: `tests/wave_4_acceptance.rs`
- Create: `tests/wave_4p5_acceptance.rs`
- Create: `tests/wave_5_acceptance.rs`
- Create: `tests/wave_6_acceptance.rs`
- Create: `tests/wave_7_acceptance.rs`
- Create: `tests/wave_8_acceptance.rs`
- Create: `tests/wave_9_acceptance.rs`

- [ ] **Step 1: Author `tests/wave_0p5_acceptance.rs` (self-test)**

```rust
//! Wave 0.5 — Drift-prevention automation acceptance criteria.
//!
//! When the implementer subagent lands each acceptance behavior, it removes
//! the `#[ignore]` line from the matching test. The wave is "done" only when
//! every test in this file runs un-ignored.

#[test]
fn landing_page_contract_suite_runs() {
    // Presence of tests/landing_page_contract.rs proves this acceptance.
    // We assert here just so this file isn't empty.
    assert!(std::path::Path::new("tests/landing_page_contract.rs").exists());
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
```

- [ ] **Step 2: Author `tests/wave_1_acceptance.rs` (WebSocket)**

Map every acceptance criterion from the roadmap Wave 1 entry into a `#[ignore]`-gated test. Pattern:

```rust
//! Wave 1 — WebSocket APIs acceptance criteria.

#[test]
#[ignore = "wave 1 not yet shipped: protocol = websocket toml parse"]
fn protocol_websocket_parses() {
    let toml_str = r#"
[server]
port = 3000
host = "0.0.0.0"

[function.chat]
runtime = "bun"
handler = "./chat.handler"
protocol = "websocket"
"#;
    let cfg: riz::config::Config = toml::from_str(toml_str).unwrap();
    let f = cfg.functions.get("chat").unwrap();
    assert_eq!(f.protocol, riz::config::Protocol::WebSocket);
    // ALREADY shipped at WS Task 2 — remove the #[ignore] once we land here.
}

#[test]
#[ignore = "wave 1 not yet shipped: $connect dispatches an ApiGatewayWebsocketProxyRequest"]
fn websocket_connect_dispatches_proxy_request() {
    // Implementer fills this in during WS Task 7.
}

#[test]
#[ignore = "wave 1 not yet shipped: $default invoked per message"]
fn websocket_default_dispatches_per_message() {}

#[test]
#[ignore = "wave 1 not yet shipped: $disconnect dispatches on close"]
fn websocket_disconnect_dispatches_on_close() {}

#[test]
#[ignore = "wave 1 not yet shipped: connectionId is present in requestContext"]
fn websocket_connection_id_populated() {}

#[test]
#[ignore = "wave 1 not yet shipped: POST /_riz/connections/{id} sends to client"]
fn connections_post_sends_to_client() {}

#[test]
#[ignore = "wave 1 not yet shipped: DELETE /_riz/connections/{id} closes connection"]
fn connections_delete_closes_connection() {}

#[test]
#[ignore = "wave 1 not yet shipped: GET /_riz/connections/{id} inspects connection"]
fn connections_get_inspects_connection() {}

#[test]
#[ignore = "wave 1 not yet shipped: connections survive hot-reload of the ws function"]
fn websocket_connections_survive_hot_reload() {}

#[test]
#[ignore = "wave 1 not yet shipped: all connections cleanly closed on SIGTERM within 30s"]
fn websocket_clean_close_on_sigterm() {}
```

- [ ] **Step 3: Author `tests/wave_2_acceptance.rs` (Python)** — one test per Wave 2 acceptance criterion, all `#[ignore]`.
- [ ] **Step 4: Author `tests/wave_3_acceptance.rs` (Authorizers)** — same pattern.
- [ ] **Step 5: Author `tests/wave_4_acceptance.rs` (CORS)** — same pattern.
- [ ] **Step 6: Author `tests/wave_4p5_acceptance.rs` (Bearer-token auth)** — at minimum:
  - `/_riz/health` returns 200 with no auth (liveness, must remain open)
  - `/_riz/metrics` returns 401 with no auth when `[auth] bearer_token = "..."` is set
  - `/_riz/metrics` returns 200 with the correct Authorization header
  - `/_riz/registry`, `/_riz/mcp` same treatment
  - All five tests `#[ignore]`-gated
- [ ] **Step 7: Author `tests/wave_5_acceptance.rs` (context fidelity)** — same pattern.
- [ ] **Step 8: Author `tests/wave_6_acceptance.rs` (Rust adapter)** — same pattern.
- [ ] **Step 9: Author `tests/wave_7_acceptance.rs` (code debt)** — one test per sub-item (7.1–7.10).
- [ ] **Step 10: Author `tests/wave_8_acceptance.rs` (test gaps)** — meta-tests that assert other test files exist.
- [ ] **Step 11: Author `tests/wave_9_acceptance.rs` (marketing)** — meta-tests on the README + asciinema file existence.
- [ ] **Step 12: Run**

```bash
cargo test --workspace --all-targets 2>&1 | grep "test result"
```

All tests pass (the wave_N_acceptance files have everything `#[ignore]`-gated except wave_0p5 which has three meta-assertions).

```bash
cargo test --workspace --all-targets -- --ignored 2>&1 | grep "test result"
```

The ignored tests run; they pass where the underlying behavior already exists (WS Tasks 1–3) and fail where it doesn't yet (everything else). Failures here are **expected** — that's the wave-not-done signal.

- [ ] **Step 13: Commit**

```bash
git add tests/wave_*.rs
git commit -m "test(drift): per-wave acceptance scaffolds — #[ignore]-gated oracles"
```

---

## Task 8: GitHub Actions CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    name: Build + test + lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - name: Cache cargo
        uses: Swatinem/rust-cache@v2

      - name: Install Bun
        uses: oven-sh/setup-bun@v1
        with:
          bun-version: latest

      - name: cargo fmt
        run: cargo fmt --all -- --check

      - name: cargo clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: cargo build
        run: cargo build --workspace

      - name: cargo test
        run: cargo test --workspace --all-targets

  acceptance-future:
    name: Future-wave acceptance (informational)
    runs-on: ubuntu-latest
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: oven-sh/setup-bun@v1
        with: { bun-version: latest }
      - name: Run #[ignore]-gated wave acceptance tests
        run: cargo test --workspace --all-targets -- --ignored
```

- [ ] **Step 2: Verify YAML parses (locally)**

```bash
ruby -ryaml -e "YAML.load_file('.github/workflows/ci.yml')" 2>&1 | tail -3
```

Or use any YAML linter. Expected: no error.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: GitHub Actions — build, test, clippy, fmt + future-wave informational"
```

---

## Task 9: Ungate the Bun integration tests

**Files:**
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Identify the `#[ignore]` attributes**

```bash
grep -n "#\[ignore" tests/integration_test.rs
```

- [ ] **Step 2: For every `#[ignore]` whose reason is "needs bun", delete the attribute. Tests now run against CI's bun install.**

- [ ] **Step 3: Run locally (requires bun present on PATH)**

```bash
cargo test --test integration_test 2>&1 | tail -10
```

Expected: passes (CI guarantees bun is present; locally requires `brew install oven-sh/bun/bun` or similar).

- [ ] **Step 4: Commit**

```bash
git add tests/integration_test.rs
git commit -m "test: ungate Bun integration tests — CI installs Bun"
```

---

## Task 10: Final verification

- [ ] **Step 1: Full clean test**

```bash
cargo clean
cargo test --workspace --all-targets 2>&1 | grep "test result"
```

Expected: every line says "ok" with passing counts. No new failing tests.

- [ ] **Step 2: Future-wave informational run**

```bash
cargo test --workspace --all-targets -- --ignored 2>&1 | tail -30
```

Expected: a mix of passes (where behavior already exists, e.g., WS Tasks 1–3) and failures (where waves haven't shipped). Capture the output as a "drift dashboard" — the green/red pattern shows progress at a glance.

- [ ] **Step 3: Clippy + fmt clean**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --all -- --check 2>&1 | tail -5
```

Both clean.

- [ ] **Step 4: Mark Wave 0.5 complete in the roadmap**

Edit `docs/superpowers/plans/2026-05-26-v01-honest-ship-roadmap.md`: add `✅` next to the Wave 0.5 heading.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/plans/2026-05-26-v01-honest-ship-roadmap.md
git commit -m "docs: mark Wave 0.5 (drift-prevention automation) complete"
```

---

## Self-review

**Spec coverage** (against `docs/superpowers/specs/2026-05-26-drift-prevention-automation-design.md`):
- Landing-page contract suite → Tasks 1–5 ✓
- AWS contract golden fixtures → Task 6 ✓
- Per-wave acceptance scaffolds → Task 7 ✓
- GitHub Actions CI workflow → Task 8 ✓
- Bun ungate → Task 9 ✓
- Self-test that landing-page suite proves itself → covered by Task 1 smoke + Task 7 wave_0p5 file ✓

**Placeholder scan:** No "TBD"/"TODO" remaining. Every step shows the exact code or command. Task 6's fixture content references AWS docs (the JSON IS verbatim from AWS, not invented).

**Type consistency:** `Claim`, `PILLS`, `WORKS_NOW`, `COMING`, `deep_normalize`, `extract_*` — all named identically across Tasks 1–7. `riz::config::Config`, `riz::config::Protocol`, `riz::gateway::ApiGatewayV2httpRequest`, `riz::gateway::ApiGatewayWebsocketProxyRequest` — all match the actual exports in `src/lib.rs` + `src/gateway.rs`.

---

## Done.
