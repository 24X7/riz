# Claims-Truth & AI-Substrate Hardening — Master Plan

> **For agentic workers:** This is a MASTER plan spanning multiple subsystems. It is intentionally
> "on paper" — phase goals, file maps, representative tasks, test strategy, and the open decisions
> that block detailed TDD breakdown. Each phase gets its own `superpowers:writing-plans` pass that
> expands it into bite-sized red/green/commit steps **at execution time** (post-compaction). Do not
> begin execution from this document — see "Stop Gate" at the bottom.

**Goal:** Make every capability claimed on riz.dev provably true via real product/integration tests
("hold the line"), lead with the agent-substrate story, and build the AI-native depth (single-path
observability with token-aware tracing, WorkOS/Clerk auth, Claude Agent SDK examples, hardened WASM
with brokered resource access) that the positioning promises.

**Architecture principle — the website is the spec.** A claim on the page is a contract. Each claim
maps to a named product/integration test. False claim ⇒ red test ⇒ either build the feature or change
the copy. No claim ships untested.

**Tech stack:** Rust (riz host, Ratatui TUI, Clap), `cargo nextest` only. OpenTelemetry (OTLP) for
the single observability path. Bun/Node/Python/Rust/WASM runtimes. Claude Agent SDK + Anthropic API
for AI examples.

---

## Current-state assessment (verified 2026-06-09)

| Area | Reality today | Gap vs. site claims |
|---|---|---|
| Claim tests | `tests/landing_page_contract.rs` was **gutted** to 2 cheap HTML checks; comment says marketing correctness is "a human/PR concern." 498 `#[test]`s exist across 47 integration files. | No product-level claim verification. Garbage per user. |
| Observability | `src/metrics.rs` = StatsD→Datadog via `cadence` (UDP). `DatadogConfig` only. | **No OpenTelemetry, no CloudWatch/X-Ray, no traces/spans, no token metrics.** Site claims "OTel exporter w/ W3C Trace Context" — **FALSE today.** |
| Token metrics | none | Site implies AI-FinOps / token attribution — not implemented. |
| Auth | `src/auth/{jwt,authorizer,request}.rs`, JWKS, `examples/riz.jwt.toml`. JWT validate works. | No WorkOS/Clerk samples or site positioning; not verified against their token shapes. |
| WASM | `examples/lambdas/echo-wasm` (echo only), wasm32-wasip1 under wasmtime, deny-by-default sandbox. | No resource access (Postgres/Dynamo/Supabase/Neon/S3); no host capability broker. |
| AI examples | `chat`, `chat-python`, `chat-rust` (gateway round-trips). | No agent-loop / tool-use / Claude Agent SDK examples. |
| `vs` page | Separate `web/vs/index.html`. | User wants it folded into main, repositioned so it doesn't distract from production + AI lead. |
| Founder quote | Present in `#author`; about economics + local dev loop. | Doesn't touch the agent-substrate angle (the most compelling piece). |
| Docs | 25 files under `docs/` incl. stale `status/2026-05-29-session-state.md`, many superpowers plans/specs. | "AI doc slop" cleanup + a guard test to keep it clean. |
| CONTRIBUTING | 284-line `CONTRIBUTING.md` with build/test/curl/wrk commands; `benches/{run-bench.sh,README.md}` use `wrk`. | Commands not verified runnable-from-clone (flag ordering suspect); needs complete test+`wrk` command set + a guard so they can't rot. → **Phase 1c.** |
| TUI | `src/tui/{app,widgets,snapshot}.rs`, `--dev` only. | Needs token/trace metrics surfaced during dev. |

---

## Execution order (rationale)

The requested items were unordered. Best execution sense, by dependency:

0. **Homepage repositioning & content finalization** — lock the narrative/claims first, so we don't
   write claim-tests against copy we're about to rewrite. Cheap, no backend deps, sets the target.
1. **Test-trust foundation: claims registry + suite audit + slop cleanup guard** — with claims now
   stable, map each to a real product/integration test and replace the garbage landing test. This
   turns currently-false claims (OTel, token metrics) **red**, which drives phases 2–5. Also audits
   the existing 498 tests for trustworthiness and adds the doc/code cleanliness guard.
2. **Observability single-path + token-aware tracing + dev TUI** — the largest real engineering and
   the biggest batch of red claims. Build it; greens those tests.
3. **Auth: JWT + WorkOS + Clerk** — samples, verification against real JWKS/test tenants, site
   positioning. (Needs signups — see Accounts ledger.)
4. **AI-native examples incl. Claude Agent SDK** — showcase depth on top of a solid gateway/MCP.
5. **WASM hardening + brokered resource access** — strongest design unknown; partly roadmap.
6. **Roadmap consolidation + hardening backlog** — fold #9/#12/etc. into one ranked harden list.

Cross-cutting artifacts (built alongside): **Accounts ledger** (needed before phase 3), **Capability
baseball card**, **"How we built this" meta-doc**.

---

## Phase 0 — Homepage repositioning & content lock

**Goal:** Finalize the page so claims are stable before we test them.

**Files:**
- Modify: `web/index.html` (sections `#status` features, `#author`, new folded `vs` content)
- Delete/redirect: `web/vs/index.html` (fold into main; keep URL as anchor or 301 note)
- Test: `tests/landing_structure.rs` (new — structural, not claim-truth; that's Phase 1)

**Work:**
1. **Fold `vs` into main, de-emphasized.** Move the comparison content into a lower section (after
   features, before Support), styled subtle/secondary so it does not compete with the production +
   AI-lead hero. Keep `web/vs/` as a thin redirect or remove and update `sitemap.xml`/nav.
2. **Reorder Features (#5/#4 of request):** lead with **Agent & AI Integration**, then **LLM
   Gateway**, then the rest. Update the feature-grid source order in `#status`.
3. **Rewrite founder quote (#3)** to center the **agent substrate** thesis. Draft below, pending
   Chris's tweak:

   > "I spent twenty years watching teams pour their logic into APIs — then watched all of it
   > become invisible to the agents that now do the work. riz flips that: the functions you already
   > have become an agent's hands the moment the binary boots, no rewrite, no cloud account. The API
   > layer everyone already built is the substrate the agent era runs on. I wanted to own that
   > substrate, locally, end to end — so I built it."
   > — Chris Rizzuto

4. **"Coming soon" ribbon mechanism (decided).** Capabilities we're positioning but haven't shipped/
   proven are shown **visibly greyed with a small "coming soon" ribbon** rather than hidden. Add a
   reusable `.coming-soon` card state + ribbon component to the design system; each such element
   carries a `data-claim="<id>"` tying it to a `coming-soon` registry entry (Phase 1 enforces the
   mapping both ways).
5. Structural test (`tests/landing_structure.rs`): assert feature order (Agent & AI Integration
   first, LLM Gateway second), the folded comparison section exists and sits after features, `vs/`
   no longer linked as a separate top-nav page, and every `.coming-soon`/ribboned element has a
   `data-claim` that resolves in the registry.

**Decisions/detail needed:**
- **Founder-quote final wording** — draft above; Chris to approve/tweak.
- **`vs` fate:** redirect vs. delete. (Default: keep `/vs` 301→`/#compare` anchor for inbound links.)
- Which current capabilities are `proven` vs. `coming-soon` at first ship — produced as the registry
  in Phase 1 (e.g. OTel/token-metrics/WorkOS/Clerk/WASM-resources start `coming-soon`-ribboned and
  lose the ribbon as Phases 2–5 turn their tests green).

---

## Phase 1 — Test-trust foundation (claims registry, suite audit, cleanliness guard)

**Goal:** Replace the gutted landing test with real product/integration tests that prove each website
claim; raise the trust floor of the whole suite; add a guard that keeps docs/code clean.

**Files:**
- Create: `tests/claims/registry.toml` — machine-readable claim → test mapping (claim id, page text,
  the test fn that proves it, status: `proven|red|copy-only`).
- Create: `tests/claims_truth.rs` — drives the registry: every `proven` claim must have a passing
  named test; every page claim must appear in the registry (no orphan marketing); `red` claims are
  allowed only with an explicit linked roadmap item.
- Rewrite: `tests/landing_page_contract.rs` → thin wrapper that (a) extracts claim-bearing copy and
  (b) asserts each is registered. (Kills the "human review only" stance.)
- Create: `tests/trust_audit.rs` — fails on test anti-patterns (tautologies like `assert!(true)`,
  blanket `#[ignore]` without a tracking ref, tests that only assert on mocks).
- Create: `tests/repo_cleanliness.rs` — the slop guard (see below).
- Create: `xtask` or `scripts/claims-sync` — helper to diff page copy vs. registry.

**Work:**
1. **Extract the claims.** Enumerate every capability assertion on `web/index.html` (hero, vision,
   MCP, config, features grid, observability, auth, proof). Assign each a stable `claim-id`.
2. **Classify** each: `proven` (real test exists & passes), `coming-soon` (real capability we intend
   but haven't built/proven yet — rendered **visibly greyed with a "coming soon" ribbon** on the
   page, see Phase 0), or `copy-only` (subjective marketing, exempt but explicitly marked).
   **Enforcement rule (this is how we "hold the line"):** every claim that is NOT ribboned
   `coming-soon` and NOT `copy-only` MUST be `proven` (have a passing product test). Conversely,
   anything carrying the `coming-soon` ribbon MUST be registered as not-yet-proven and linked to a
   roadmap item — it may not silently become a normal claim. `tests/claims_truth.rs` fails on either
   violation, so a greyed feature can't quietly go live unproven and a live claim can't exist without
   a green test.
3. **Back each `proven`/to-be-proven claim with a REAL product test** — exercise the running runtime,
   not HTML. Examples:
   - "91k req/s · p99<1ms" → a perf assertion gate (already partially in `perf_ws_load.rs`); wire it
     to the claim-id with a documented environment.
   - "five runtimes" → boot each runtime, round-trip a request (parity tests already exist; map them).
   - "every function is an MCP tool" → `riz mcp inspect` lists each configured function as a tool
     (extend `cli_mcp_inspect.rs`).
   - "OpenTelemetry exporter / token attribution" → **currently red**; the test is written first
     (TDD) and drives Phase 2.
   - "JWT authorizers (RS256/ES256 vs your IdP's JWKS)" → real JWKS validation test (extend
     `authorizer_integration.rs`); WorkOS/Clerk variants land in Phase 3.
4. **Suite trust audit.** Review the 498 existing tests for: over-mocking, assertions on fixtures
   instead of behavior, `#[ignore]` drift, and "passes immediately / proves nothing" smells. Produce
   `docs/superpowers/specs/2026-06-09-test-trust-audit.md` listing findings + fixes. Harden the weak
   ones into real integration coverage.
5. **Repo cleanliness guard (`tests/repo_cleanliness.rs`).** Define "clean" concretely and enforce:
   - No orphaned/stale status dumps (e.g. `docs/status/*-session-state.md` older than N or not
     referenced from an index).
   - No AI-slop markers (`TODO(claude)`, dangling "as an AI", "Here's the", doubled headers, etc.)
     in committed docs/code.
   - Every `docs/superpowers/plans/*.md` has a matching spec or is marked archived.
   - `MEMORY.md` index entries resolve to existing files.
   The test fails on violation, so the repo "holds the line" once cleaned.

### Phase 1c — CONTRIBUTING.md & docs commands must actually run

**Goal:** Every command in `CONTRIBUTING.md` (and other command-bearing docs) runs verbatim from a
fresh clone; provide the complete test + benchmark command set, including the `wrk` flow (script-
wrapped); guard it so it can't rot.

**Files:**
- Modify: `CONTRIBUTING.md` (284 lines today — fix/complete commands)
- Modify/verify: `benches/run-bench.sh`, `benches/README.md` (complete copy-paste wrk flow)
- Create: `scripts/bench.sh` (one-command `script`-wrapped wrk run: build release → boot riz →
  warm → `wrk -t4 -c20 -d20s --latency …` → teardown → print the measured req/s + p99)
- Create: `tests/docs_commands_runnable.rs` (extracts fenced shell commands tagged for verification
  and runs a safe subset; asserts exit 0)

**Work:**
1. **Audit every command** in CONTRIBUTING against the real CLI surface. Known suspects to verify:
   - Flag/subcommand ordering: doc shows `cargo run -- --dev run` and `cargo run -- --dev --port
     4000 run` — confirm the actual Clap surface accepts global flags *before* the subcommand, or fix
     to the true form (`riz run --dev`, per repo memory "TUI driven only by --dev"). Reconcile doc ↔
     CLI; if they disagree, the **CLI is source of truth** and the doc is wrong.
   - `cargo nextest run --test landing_page_contract` — updates when Phase 1 rewrites that test.
   - The curl smoke block (ping/accounts/events/echo-python) must match `examples/riz.dev.toml`
     routes as they exist today.
   - Install/prereq block (rustup, bun, wrk) — verify each on a clean machine path.
2. **Complete the test command set:** a single authoritative section — full suite (`cargo nextest
   run`), focused filters, the scaffold/e2e, and the **benchmark** via `scripts/bench.sh` (or the
   documented raw `wrk` two-terminal flow). Make the wrk numbers reproducible (cite the
   benches/README method).
3. **Guard test (`tests/docs_commands_runnable.rs`):** parse fenced ```` ```bash ```` blocks in
   CONTRIBUTING that are marked verifiable (e.g. a `# @verify` marker), execute the fast/safe ones in
   a temp dir, assert success. Long/networked ones (wrk, installs) are smoke-skipped in CI but
   shape-checked (binary exists, flags parse). This makes "the docs run" a CI contract.

**Decisions/detail needed:**
- Which CONTRIBUTING commands are CI-executed vs. shape-checked only (wrk/installs are heavy/
  networked). Proposal: execute build/test/CLI/curl-smoke; shape-check wrk + installers.
- Whether `scripts/bench.sh` wraps `wrk` directly or via `script -q` for TTY-faithful capture
  (the user suggested "script with wrk"). Proposal: `scripts/bench.sh` runs wrk directly; offer a
  `--tty` mode that wraps in `script -q` for terminal-accurate output.

**Decisions/detail needed (perf):**
- **Perf-claim environment:** what hardware/CI profile makes "91k req/s · p99<1ms" a deterministic
  gate vs. a documented-but-not-CI-gated benchmark? (Proposal: gate a conservative floor in CI, cite
  the headline number with a reproducible `benches/` recipe.) This claim's test (Phase 1) uses
  `scripts/bench.sh`.
- **Cleanliness ruleset:** exact slop-pattern list + doc-freshness policy (which dirs are
  authoritative vs. archive). Needs a short ruleset spec.
- **`red`-claim policy:** do we allow shipping a `red` claim if linked to a roadmap item, or must the
  page only state `proven` capabilities? (Proposal: page states only `proven`; "coming" items live in
  a clearly-labeled Roadmap section, themselves untested.)

---

## Phase 2 — Observability: one path (OTLP), token-aware tracing, dev TUI

**Goal:** Replace StatsD-only with a single OpenTelemetry pipeline that fans out to Datadog and
CloudWatch/X-Ray; capture token utilization tied to request → function-call → chat-completion spans;
surface it live in `--dev`.

**ARCHITECTURE (decided): dedicated telemetry process, same binary, fully isolated.** The
observability pipeline runs as its **own child process** that the `riz` binary self-spawns (e.g.
`riz __telemetry` internal subcommand / `current_exe()` re-exec), **separate from the core host event
loop and from every function process pool.** The host emits telemetry events over a non-blocking IPC
channel (UDS or pipe) to this process, which owns OTLP encoding + all exporters. **Resiliency
contract:** if the telemetry process is slow, backpressured, or crashes, the host keeps serving
requests with zero added latency (bounded queue, drop-or-sample on overflow, supervised restart).
This keeps the single-binary story *and* guarantees telemetry can never degrade host or function
performance.

**Files:**
- Create: `src/observability/mod.rs` (host-side emitter + IPC client, bounded non-blocking queue),
  `src/observability/process.rs` (the dedicated telemetry worker: IPC server + OTLP pipeline +
  exporters + supervised lifecycle), `src/observability/otel.rs` (OTLP encode + Datadog / CloudWatch-
  X-Ray exporters), `src/observability/ipc.rs` (wire format + channel)
- Modify: `src/main.rs` (internal `__telemetry` subcommand / re-exec spawn + supervisor),
  `src/metrics.rs` (deprecate cadence/StatsD path), `src/config.rs` (`[telemetry]` replacing
  `[datadog]`), `src/server.rs` (open request spans, push events to the emitter — never block on it),
  gateway module (chat-completion spans + token attrs), `src/tui/{app,widgets,snapshot}.rs`
- Test: `tests/telemetry_process_isolation.rs` (kill/stall the telemetry process → host throughput &
  p99 unaffected; supervised restart), `tests/telemetry_otlp.rs`, `tests/telemetry_token_spans.rs`,
  `tests/tui_token_panel.rs`

**Work:**
0. **Spawn the dedicated telemetry process** from the same binary; wire the host→telemetry IPC with a
   bounded queue and overflow policy (drop/sample, never block the host). Supervise + restart it.
   First test is the **isolation guarantee**: stalling or killing the telemetry process leaves host
   req/s and p99 within noise (reuse the perf harness).
1. **One path = OTLP.** The telemetry process emits OTLP traces+metrics (and optionally logs) from a
   single tracer/meter. Fan-out via exporter config — **not** separate bespoke integrations.
2. **Datadog + CloudWatch/X-Ray** as OTLP sinks (DD OTLP intake; X-Ray/CloudWatch via OTLP→X-Ray).
   Single `[telemetry]` config block selects enabled exporters; one data model underneath.
3. **Token-aware spans.** Each inbound request opens a root span; each function invocation a child
   span; each gateway chat-completion a grandchild span carrying `gen_ai.usage.input_tokens`,
   `gen_ai.usage.output_tokens`, model, provider (follow OTel GenAI semantic conventions). Token
   totals roll up the span tree so a request's full token cost (incl. multi-step tool/agent chains)
   is attributable.
4. **Dev TUI panel.** In `--dev`, show per-request and per-function token utilization and latency
   percentiles (P50/P75/P90/P95/P99) live, plus the active trace/span chain.
5. **Claim tests:** OTLP export emits expected spans/attrs (in-memory exporter assertion); token
   attribution sums correctly across a tool/agent chain; TUI snapshot shows the token panel.

**Decisions/detail needed:**
- ~~In-process vs. collector sidecar~~ **RESOLVED:** dedicated telemetry **process** inside the same
  binary, isolated from host + functions (see Architecture above). Detail to spec at execution:
  IPC transport (UDS vs. pipe), wire format, queue bound + overflow policy, and the supervisor/
  restart semantics.
- **CloudWatch/X-Ray mapping:** X-Ray's segment model ≠ OTLP spans 1:1. Confirm via ADOT-style
  mapping or AWS OTLP endpoint. Detail spec required.
- **GenAI semantic conventions version** to pin (they're evolving).
- **Backward compat:** `[datadog]`/StatsD — keep as a legacy exporter or remove? (Greenfield rule
  says we can remove; but Datadog-via-OTLP replaces it cleanly. Proposal: remove StatsD, Datadog via
  OTLP.)

---

## Phase 3 — Auth: JWT + WorkOS + Clerk

**Goal:** Demonstrate standards-based JWT auth (already working) and prove compatibility with WorkOS
and Clerk, with sample configs, real integration tests, and site positioning.

**Files:**
- Create: `examples/riz.workos.toml`, `examples/riz.clerk.toml`
- Modify: `src/auth/jwt.rs` (only if token-shape gaps found), `web/index.html` (#auth positioning)
- Test: `tests/auth_workos.rs`, `tests/auth_clerk.rs` (real JWKS + token validation)

**Work:**
1. Verify our JWT/JWKS validates WorkOS- and Clerk-issued tokens (RS256/ES256, JWKS rotation, `iss`/
   `aud`/`exp`). Fix any shape gaps.
2. Sample configs pointed at each IdP's JWKS; document the 3-line setup.
3. Integration tests against real test-tenant tokens (requires signups — see ledger). Where live
   tenants aren't viable in CI, test against captured JWKS + minted test tokens with the tenant's
   signing key.
4. Site: add WorkOS + Clerk to the auth section as proven integrations.

**Decisions/detail needed:**
- **CI secret strategy** for WorkOS/Clerk test tenants (live calls vs. recorded JWKS + locally-minted
  tokens). Proposal: recorded-JWKS + minted tokens for determinism; one nightly live smoke.
- Confirm WorkOS/Clerk handles/tenants to create (ledger).

---

## Phase 4 — AI-native examples (incl. Claude Agent SDK)

**Goal:** Examples that show riz as the agent substrate, including the latest Claude Agent SDK.

**Files:**
- Create: `examples/lambdas/agent-tooluse/` (handler exposing tools an agent calls via MCP)
- Create: `examples/agent-sdk/` (a Claude Agent SDK script that connects to riz's MCP endpoint and
  drives the example functions as tools)
- Create: `examples/lambdas/rag-*` / `examples/lambdas/agent-loop-*` (TBD set)
- Test: `tests/examples_agent.rs` (boot riz, run the agent example end-to-end against mock/real
  provider), map to claim-ids.

**Work:**
1. A flagship example: Claude Agent SDK → riz `/_riz/mcp` → calls real handler tools → returns. Use
   `claude-opus-4-8`, adaptive thinking, streaming, per the claude-api guidance.
2. 2–3 more AI patterns (tool-use loop, multi-step chain showing token attribution from Phase 2).
3. Wire into `examples/demo.py` so the demo showcases the agent path live.

**Decisions/detail needed:**
- Which Agent SDK language (Python vs. TS) for the flagship. Proposal: Python (matches demo + skill
  examples).
- Provider for CI (mock vs. gated real Anthropic key). Proposal: mock in CI, real behind env var.
- The exact example set (this whole phase needs a short design spec).

---

## Phase 5 — WASM hardening + brokered resource access

**Goal:** Stronger WASM examples and a design for accessing external resources (Postgres, DynamoDB,
Supabase, Neon, S3, etc.) from the sandbox **without breaking host resiliency**.

**Files:**
- Create: `examples/lambdas/wasm-*` (richer than echo: real compute, structured I/O)
- Create: `docs/superpowers/specs/2026-06-09-wasm-resource-broker-design.md` (the design — mostly
  roadmap)
- Modify (later, roadmap): host capability-broker module exposing vetted host functions to guests.
- Test: `tests/wasm_examples.rs`, `tests/wasm_broker_*` (when broker lands)

**Work:**
1. **Now:** ship 1–2 stronger WASM examples (real logic, JSON I/O, deterministic), proving the
   runtime beyond echo. Map to the "WASM runtime" claim.
2. **Design (roadmap):** a host-mediated **resource broker** — guests can't open arbitrary sockets;
   instead they request capabilities (a Postgres query, an S3 get, a Dynamo call) that the host
   executes under policy, with allow-lists, timeouts, and per-call limits so a guest can't exhaust or
   crash the host. One brokered interface, many backends (Postgres/Neon/Supabase share the PG wire;
   Dynamo/S3 via AWS SDK on the host side).

**Decisions/detail needed (large):**
- **Broker interface model:** WASI preview2 component interfaces vs. custom host functions vs. a
  syscall-style capability API. Needs a design spike.
- **Resiliency policy:** per-call timeouts, concurrency caps, memory/CPU rlimits already exist — how
  brokered I/O composes with Landlock/rlimits.
- **Which backends in v1** vs. roadmap (proposal: Postgres-wire first → covers Neon/Supabase; S3 +
  Dynamo second).
- This phase is mostly **roadmap**; only the richer examples are near-term.

---

## Phase 6 — Roadmap consolidation + hardening backlog

**Goal:** One ranked "battle-test & harden" backlog, absorbing the request's #9/#12 and Phase-2..5
roadmap spillover.

**Files:**
- Modify: `docs/plans/v1-roadmap.md` (or a new `docs/plans/2026-06-09-harden-backlog.md`)

**Work:** consolidate: WASM resource broker, more AI examples/agent patterns, OTel exporter
hardening, auth CI strategy, perf-claim CI gating, cleanliness ruleset, multi-provider gateway
resilience, recording/replay depth, etc. Rank by value × risk.

---

## Cross-cutting artifacts

### A. Accounts ledger (needed before Phase 3) — `docs/ACCOUNTS-TO-PROVISION.md`
Living list of services to sign up for to test/operate, with purpose and what's needed:
- **Clerk** — auth test tenant + JWKS (Phase 3)
- **WorkOS** — auth test tenant + JWKS (Phase 3)
- **Datadog** — OTLP intake + API key (Phase 2)
- **AWS** — CloudWatch/X-Ray + IAM for OTLP/X-Ray (Phase 2)
- **Neon / Supabase** — Postgres-wire targets (Phase 5)
- **AWS S3 + DynamoDB** — brokered resource targets (Phase 5)
- **Anthropic API key** — Claude Agent SDK examples (Phase 4)
- **OpenAI key** (optional) — gateway compat tests
- **Payments to Chris:** GitHub Sponsors (enable), Buy Me a Coffee handle, **crypto wallet addresses
  (ETH/BTC/SOL)** to replace site placeholders; consider Stripe/Open Collective if recurring fiat
  beyond Sponsors is wanted.

### B. Capability baseball card — `docs/CAPABILITY-CARD.md` (+ optional web component)
One ultra-clean card: what riz is, the five runtimes, agent-native/MCP, gateway, auth, observability,
WASM, perf headline, license. Designed to be screenshot/share-ready. Mirror onto the site as a
compact "stat card" if desired.

### C. "How we built this" meta-doc — `docs/HOW-WE-BUILD.md`
Narrative + playbook of the build method: persistent **memory/context**, **tools**, **`/loop`**
autonomous build sessions, **`/btw`** side-affirmations, **planning + task lists**, and **superpowers**
(parallel-agent "party mode", TDD, writing-plans, etc.). Doubles as marketing ("built by agents,
for agents") and as the team's working method. User wants us using superpowers *more* — bake that in.

---

## Stop Gate

Per the request: **this plan is now on paper. Stop here and compact before proceeding.** After
compaction, each phase gets its own `superpowers:writing-plans` expansion into bite-sized
red/green/commit TDD steps, executed via `superpowers:subagent-driven-development` (party mode) with
review between tasks. Phase 0 → 1 first (lock narrative, then make the suite hold the line), then
2 → 6 by dependency.

### Resolved decisions (locked 2026-06-09)
- **Observability runs as a dedicated telemetry PROCESS** inside the same binary, isolated from host
  + function pools, with a host-resiliency contract (can't slow/crash serving). (was "in-process vs.
  collector")
- **Unbuilt-but-positioned capabilities ship visibly greyed with a "coming soon" ribbon** (not
  hidden); enforced both ways by the claims registry. Live (non-ribboned) claims must be `proven`.
- **Founder quote** reframed to the agent-substrate thesis (draft in Phase 0, pending Chris's tweak).

### Open decisions to resolve before/at execution (consolidated)
1. Founder-quote final wording — approve/tweak the Phase 0 draft.
2. `vs` page: 301-redirect vs. delete.
3. Perf-claim CI gating profile + the cleanliness ruleset spec.
4. Telemetry IPC detail: transport (UDS vs. pipe), queue bound + overflow policy, supervisor restart.
5. StatsD/Datadog legacy path: remove vs. keep (proposal: remove; Datadog via OTLP).
6. CloudWatch/X-Ray OTLP→segment mapping approach.
7. Auth CI: recorded-JWKS+minted tokens vs. live tenants.
8. Agent SDK flagship language (Python proposed) + CI provider strategy.
9. WASM broker interface model + which backends are v1 vs. roadmap.
10. CONTRIBUTING: which commands CI-execute vs. shape-check; `scripts/bench.sh` raw-`wrk` vs.
    `script -q`-wrapped; reconcile CLI flag/subcommand ordering (CLI is source of truth).
