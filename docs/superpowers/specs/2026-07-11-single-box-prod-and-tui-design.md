# Single-Box Production + Dev/TUI Polish — Design & Plan

Status: **approved (scope + shape)**, pending spec review.
Date: 2026-07-11. Branch: `feat/single-box-prod-tui`.

This is the tracked plan. Each Core item ships as its own PR, validated before
merge (tests; Linux-only paths via the Docker aarch64 workflow — see the
"Validation" note per item and `project_production_readiness` memory).

---

## 1. Assessment (where we are)

**Feature surface.** Six runtimes (Bun/Node/Python/Rust/Go/WASM) · AWS Lambda
HTTP/WS · MCP server (`/_riz/mcp`) · OpenAI-compatible LLM gateway (`/_riz/v1/*`)
· A2A agent (`/_riz/a2a`) · WASM capability broker + fail-closed guards · static
serving · S3 hot-swap deploy · JWT/bearer/CORS auth · OTLP telemetry · response
cache · Prometheus `/_riz/metrics` · `/_riz/health` + `/_riz/ready`. CLI:
`run / routes / deploy / mcp inspect / a2a send / doctor / init (7 templates) /
agent-card`.

**NFRs — closed since the June assessments** (do not re-open): WASM proofs now
build in CI; crash-breaker dead-code fixed; macOS silent-sandbox now warns;
Power-of-10 (253 sites → 0, no reachable panic on the request path); JWKS
fetch-amplification and reflected-origin credentialed-CORS fixed; saturation
metrics; readiness probe; seccomp-BPF worker blocklist; release binaries exist
(v0.1.0 via cargo-dist); README test count corrected.

**NFRs — still open (single-box relevant):**
- No per-caller identity/keys/quotas/rate-limit; admin plane is one static bearer.
- No structured audit log (deploy/config/auth events).
- Latency is a Prometheus *summary* (not cross-instance aggregatable).
- The 91k req/s figure is an ungated microbenchmark.
- Deploy: no history/audit endpoint, no signed artifacts.

**Out of scope this cycle (enterprise/fleet — deferred):** HA / multi-node /
shared WS backplane, externalized durable state, OAuth 2.1 on MCP, mTLS,
KMS/Vault, cgroups + egress isolation. These are the paid "fleet" seam.

**Local dev scenario.** Strong: `init` (7 templates), `doctor` (10 checks),
`routes`, `mcp inspect`, `--dev` TUI, hot-reload, headless logs. Friction:
README/site quick-start still says `cargo install --git` + "binaries coming"
(**stale** — v0.1.0 binaries exist); not on crates.io/npm/Homebrew; no
`wasm-http` template.

**TUI.** 4 tabs (Routes+logs, Processes+host, Cache, Tokens); tab-nav + quit;
scroll/select only on Routes. Gaps: no scroll/filter on other tabs, no log
search/level-filter, no per-function drill-down, no invocation inspector, no
saturation display, no help overlay.

---

## 2. Plan

### W1 — Edge robustness

**W1.1 Per-caller API keys + rate limiting / quotas.**
- *Gap:* the whole surface is one shared static bearer; no per-caller identity,
  no per-caller rate limit → a single noisy/hostile caller isn't containable.
- *Design:* a `[api_keys]` config block mapping key → { name, rate limit
  (req/s, burst), optional per-key scopes }. Edge middleware resolves the key
  (header), applies a token-bucket admission (bounded, in-memory, per-key), and
  429s on exceed with `Retry-After`. Preserves the existing bearer as the
  default when no keys are configured (no breaking change). Bounded memory
  (rule 3): fixed key set from config, one bucket per key.
- *Validation:* unit tests for bucket refill/burst/exceed→429; integration test
  that key A exhausting its bucket doesn't affect key B; fail-closed on unknown
  key when keys are configured.
- *PR:* `feat(edge): per-caller API keys + token-bucket rate limiting`.

**W1.2 Structured audit log.**
- *Gap:* no tamper-evident record of who deployed/reloaded/authorized.
- *Design:* an `audit` event type emitted as structured JSON (tracing target
  `riz.audit`) at: deploy (who/what/result), config hot-reload (diff summary),
  auth decision boundaries (allow/deny + principal, not the token). On by
  default to the existing log sink at a dedicated `riz.audit` target so
  operators can route it with a log filter. No new sink infra this cycle
  (durable/remote audit sinks are fleet scope).
- *Validation:* tests asserting an audit event is emitted (captured via a test
  tracing layer) for a deploy and a denied auth, with the expected fields and
  no secret material.
- *PR:* `feat(ops): structured audit log for deploy/config/auth events`.

### W2 — Observability completion

**W2.1 Latency histogram.**
- *Gap:* `riz_latency_ms` is a Prometheus *summary* with pre-computed
  quantiles — cannot be aggregated across instances (wrong metric at scale).
- *Design:* add `riz_request_duration_seconds` as a histogram with fixed buckets
  (e.g. 1ms…10s, standard-ish). Keep the summary one release for compatibility,
  mark it deprecated in `docs/METRICS.md`. The latency window (`state.rs
  LatencyWindow`) gains bucket counters alongside the percentile sample.
- *Validation:* test that `/_riz/metrics` emits `# TYPE
  riz_request_duration_seconds histogram` with `_bucket{le=...}`, `_sum`,
  `_count`; a few recorded latencies land in the right buckets.
- *PR:* `feat(metrics): latency histogram (cross-instance aggregatable)`.

**W2.2 Perf regression guard.**
- *Gap:* the 91k req/s claim is an ungated microbenchmark.
- *Design:* a CI-gated *floor* test (not the headline number) — reuse/extend the
  deterministic throughput test so a large regression fails CI. Keep it modest
  and machine-independent (assert "≥ N req/s on the CI box" where N is a
  conservative floor, or assert p99 under a ceiling for a fixed load). Document
  that the 91k figure is a bench recipe, and the CI floor is the guard.
- *Validation:* the test itself; runs in the standard suite.
- *PR:* `test(perf): CI-gated throughput/latency floor`.

### W3 — TUI polish

**W3.1 Scroll/select on all tabs.** Extend the Routes-only scroll to Processes,
Cache, Tokens (shared selection/scroll state per tab). *Validate:* render-matrix
tests (tiny terminals) already exist; add tab-scroll unit tests where feasible.

**W3.2 Log filter/search + level filter.** A `/`-to-filter input over the log
panel; level filter (info/warn/error). *Validate:* filter-logic unit tests.

**W3.3 Saturation column.** Surface `concurrency_in_use`/limit + admission-
rejected in the Processes tab (the metric added in #50, not yet shown).
*Validate:* snapshot/unit test that the column renders the values.

**W3.4 Per-function drill-down / invocation inspector.** Enter on a selected
function → a panel of recent invocations (latency, status, route). Requires a
small bounded per-function ring of recent invocations in state (rule 3: fixed
cap). *Validate:* ring-buffer unit tests + render test.

**W3.5 Help overlay (`?`).** A modal listing keybindings. *Validate:* render
test.

*PRs:* W3 ships as 1–2 PRs (e.g. `feat(tui): scroll/filter/saturation` and
`feat(tui): invocation inspector + help`). TUI is `--dev`-only, dev-scoped.

### W4 — Dev friction

**W4.1 README/site quick-start fix.** Binaries exist (v0.1.0). Replace "coming"
+ `cargo install --git` as the *only* path with the real binary install
(shell installer) as the primary, `cargo install` as the alt. Update the site's
install/quick-start to match. Keep claims test-backed (drift guards).
*Validate:* docs; claims_truth + site_structure green.

**W4.2 `wasm-http` template.** Add a `wasm-http` scaffold to `init` (a minimal
`wasm32-wasip1` HTTP handler), so the sandboxed-WASM story has a starting point.
*Validate:* a scaffold-boot test (like the existing per-template ones) if the
harness supports wasm; else a template-structure test.

**W4.3 Error-message pass.** Improve the top failure paths: bad `riz.toml`
(point at the field), missing runtime binary (name it + install hint), port in
use (say which port + how to change). *Validate:* tests asserting the improved
messages on each failure.

---

## 3. Sequencing

1. **W4.1** (README/distribution truth) — fast, high adoption value, no risk.
2. **W2.1** (histogram) + **W3.3** (saturation column) — observability pair.
3. **W1.1** (API keys + rate limit) — the biggest single-box production gap.
4. **W1.2** (audit log).
5. **W3.1/3.2/3.4/3.5** (TUI polish).
6. **W4.2** (wasm-http template) + **W4.3** (error pass).
7. **W2.2** (perf guard) — last; least urgent.

Each merges on green (admin-merge per repo policy). Linux-only code (none
expected here except possibly error-path platform checks) validated via the
Docker aarch64 workflow before merge.

## 4. Tracking checklist

- [ ] W1.1 per-caller API keys + rate limit
- [ ] W1.2 audit log
- [ ] W2.1 latency histogram
- [ ] W2.2 perf floor guard
- [ ] W3.1 scroll all tabs
- [ ] W3.2 log filter/search
- [ ] W3.3 saturation column
- [ ] W3.4 invocation inspector
- [ ] W3.5 help overlay
- [ ] W4.1 README/site distribution truth
- [ ] W4.2 wasm-http template
- [ ] W4.3 error-message pass
- [ ] Full code review of the cycle's diff

## 5. Non-negotiables (carried from repo policy)

- Power-of-10 (`docs/SAFETY.md`) binds all new `src/` code; `safety-check.sh
  --gate` + `clippy --workspace --all-targets -D warnings` + `cargo nextest`
  (never `cargo test`) before every merge.
- Website claims only shipped+tested capabilities; register in
  `tests/claims/registry.toml` (claims_truth enforces).
- No banned self-labeling words (honest/truthful/candid).
