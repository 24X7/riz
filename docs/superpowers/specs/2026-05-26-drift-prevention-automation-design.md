# Drift-prevention Automation — Design

**Status:** approved, ready for plan
**Author:** chris@riz.dev
**Date:** 2026-05-26

## Goal

Catch landing-page-vs-code drift and AWS-contract drift at commit time, before any user files an issue. Make the SDD spec-reviewer subagent's job objective rather than judgemental by giving every wave an acceptance-test oracle.

## Why now

Three concrete drift incidents have already shipped from this codebase:

1. `riz.toml` example on the landing page used `max_concurrent` while `Config::validate()` only accepts `concurrency` → first user to `curl install | sh` hits a parse error in their first 60 seconds.
2. The hero strip listed Python + Rust as available runtimes for ~50 commits while `ProcessManager` silently fell back to Bun for both → broken handlers in prod.
3. A `Start` subcommand alias was sneaked in as "backward compat" for a renamed `Run` subcommand on a project with literally zero users.

A v0.1 viral OSS launch cannot afford a fourth incident. The cheapest fix is a small automation surface that enforces "if it's on the landing page, prove it; if it shipped, lock its shape."

## Components

### 1. Landing-page contract suite (`tests/landing_page_contract.rs`)

Pure Rust integration test. Reads `web/index.html` from the repo root. Three regex-driven extractors + three assertions:

| Extractor | Source-of-truth in code | Assertion |
|---|---|---|
| The `<pre>` block inside `#config .cc` (the embedded `riz.toml`) | none — the truth IS the parse result | TOML round-trips through `Config::from_str()` + `Config::validate()` with no error |
| The `.pill` `<span>` items inside `#config .pills` | `landing_page_contract::PILLS: &[PillClaim]` — a slice of `(label, status)` tuples maintained in the test file | Set-equal: every pill on the page has a Rust entry; every Rust entry appears on the page |
| The `.status-col li` items in the `#status` section | `WORKS_NOW: &[WorksNowClaim]` slice (each claim names a Rust test that proves the feature) and `COMING: &[ComingClaim]` slice (each names a roadmap wave or `OutOfScope`) | Set-equal both ways. Each `WorksNowClaim` test name must resolve to an existing `#[test]` function in the crate. Each `ComingClaim` wave reference must resolve to a heading in the roadmap markdown file. |

A drift move (feature column change, pill rename, runtime add/remove) requires editing both the HTML and the Rust truth slice. The test does the cross-check.

### 2. AWS contract golden fixtures (`tests/fixtures/aws/` + `tests/aws_contract.rs`)

Five JSON files sourced from AWS docs (HTTP simple GET, HTTP POST w/ body, WS `$connect`, WS message, WS `$disconnect`). Per fixture:

```rust
#[test]
fn fixture_apigw_v2_http_simple_get_round_trips() {
    let raw = include_str!("fixtures/aws/apigw_v2_http_simple_get.json");
    let req: ApiGatewayV2httpRequest = serde_json::from_str(raw).expect("deserialize");
    // Sanity asserts on known fields:
    assert_eq!(req.version.as_deref(), Some("2.0"));
    assert_eq!(req.route_key.as_deref(), Some("$default"));
    // Round-trip:
    let reserialized = serde_json::to_value(&req).unwrap();
    let original: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(deep_normalize(reserialized), deep_normalize(original));
}
```

`deep_normalize` strips fields the AWS docs include but we deliberately don't model. Document those exclusions explicitly so a future contributor knows what's intentional vs. broken.

### 3. Per-wave acceptance tests (`tests/wave_<N>_acceptance.rs`)

One file per wave (0.5, 1, 2, 3, 4, 4.5, 5, 6, 7, 8, 9). Each acceptance criterion from the roadmap becomes a `#[test]` function annotated `#[ignore = "wave N not yet shipped"]`. The implementer subagent removes the `#[ignore]` line on the same task that lands the underlying behavior. A wave is "done" only when its file runs un-ignored.

This gives:
- SDD spec-reviewer: an objective oracle, not a vibe check
- Subagents: a clear "I am done with this task" signal (one `#[ignore]` removed per task)
- Marketing: a guarantee that "Works now" lists match shipped behavior (because the landing-page contract suite cross-references the test names)

### 4. CI workflow (`.github/workflows/ci.yml`)

Single workflow, push + PR triggers:

```yaml
jobs:
  test:
    - cargo build --workspace
    - cargo test --workspace --all-targets
    - cargo clippy --workspace --all-targets -- -D warnings
    - cargo fmt --all -- --check
  acceptance-future:
    # Future-wave acceptance tests are still ignored. Run them on a separate
    # job with `continue-on-error: true` so contributors see how close each
    # wave is to "done" but a failing future test doesn't block merges.
    - cargo test --workspace --all-targets -- --ignored
    continue-on-error: true
```

Bun integration tests get `oven-sh/setup-bun` and run as part of the main `test` job — no longer `#[ignore]`-gated.

## Data flow

```
index.html  ──┐
              ├──► landing_page_contract.rs  ──► assert sets equal
PILLS/CLAIMS ─┘                                        │
                                                       ▼
                                              cargo test (CI)
                                                       ▲
roadmap.md  ──► COMING claims point at wave headings ──┘
                                                       ▲
tests/wave_N_acceptance.rs ──► implementer subagent un-ignores ──┘
```

No runtime cost — all checks live in the test suite.

## Error handling

- If the HTML file can't be read → test fails with `panic!("web/index.html missing")`. The landing page is part of the repo; missing it is a build break.
- If the regex fails to extract → test fails with a diff showing extractor input vs. expected anchors. Keeps drift visible.
- If `Config::validate()` rejects the embedded riz.toml → test fails with the validation error inline (so the contributor sees exactly what broke).
- If a `WORKS_NOW` claim points at a test that doesn't exist → compile error (we use `stringify!(fn_name)` and `pub use` to surface the symbol; missing symbol = build break).

## Testing

The drift-prevention suite tests itself in three ways:

1. A negative test in `landing_page_contract.rs` that injects a broken fixture (`web/index.fixture-broken.html`) into the extractor and asserts the assertion fails — proves the test is doing real work.
2. The AWS contract suite includes one fixture deliberately stripped of `requestContext.requestId` to prove `deep_normalize` correctly flags that field.
3. The `wave_0p5_acceptance.rs` file proves the pattern on itself before any other wave authors theirs.

## Out of scope for this design

- Visual regression of `web/index.html` rendering — out of scope; landing page styling drift is a separate concern.
- Snapshot tests of `riz --help` output — nice-to-have but `landing_page_contract` already pins the install command which is the only externally-visible CLI surface on the landing page.
- Property-based fuzzing of AWS event shapes — overkill for v0.1; revisit if a real AWS payload bug ever ships.
- Coverage thresholds — adds CI noise without catching drift.

## Acceptance criteria

- `cargo test landing_page_contract` passes against the current `web/index.html` + a `PILLS` / `WORKS_NOW` / `COMING` truth slice that exactly matches.
- `cargo test --test aws_contract` passes against five AWS fixtures.
- `tests/wave_1_acceptance.rs` through `tests/wave_9_acceptance.rs` exist, each populated with `#[ignore]`-gated tests for every acceptance criterion in the roadmap. Plus `wave_0p5_acceptance.rs` and `wave_4p5_acceptance.rs`.
- `.github/workflows/ci.yml` runs the test job + the acceptance-future job on every push.
- Removing a pill from `web/index.html` without removing it from `PILLS` fails CI.
- Adding a new "Works now" line without naming a test function fails CI.
- Changing an AWS fixture's field set fails CI.
