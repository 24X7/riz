# Independent Assessment of riz — 2026-06-12 (rerun)

**Author:** Claude Fable 5 (rerun)
**Date:** 2026-06-12
**Scope and Basis:** Independent, uncontaminated review from primary sources only; no prior assessments under `docs/assessments/` were read, searched, or referenced. Inspected: `Cargo.toml`, `README.md`, full `src/` module tree with close reads of `src/system/mcp/` (`mod.rs`, `schema.rs`, `tools.rs`), `src/observability/mod.rs`, `src/process/safety.rs`, `src/llm/cost.rs`, `src/auth/*`; the test surface (file list, `tests/claims/registry.toml`, `tests/claims_truth.rs`, `tests/mcp_schema_per_route.rs`, `tests/examples_agent.rs`, `tests/trust_audit.rs`); `examples/` (`riz.agent.toml`, `riz.prod.toml`, `agent-sdk/`, `agent-loop/`, `demo.py`); `web/index.html` (hero scenes, roadmap section, support/funding section), `web/install`; `docs/plans/v1-roadmap.md`, `docs/plans/2026-06-10-gtm-and-aeo.md`, `docs/plans/2026-06-10-harden-backlog.md`; `.github/workflows/ci.yml`; `registries/README.md`; git history (`git log`, `shortlog`). Read-only; no builds run, nothing modified or committed. Graded as a skeptical platform-engineering lead deciding whether to bet an SLA on it.

---

## 1 · Features, graded on production/enterprise-grade nature

Repo vitals first, because they frame every grade: **~17.7k lines of Rust in `src/`, ~560 test functions in-tree (248 in `tests/`, 312 in `src/`), 298 commits, one author ("24X7"), first commit 2026-05-18** — the entire project is under four weeks old. No published release binaries, no crates.io publish, zero users (by the project's own admission). That doesn't make the code bad — much of it is genuinely good — but "enterprise-grade" is a claim about miles driven, and the odometer reads zero.

### 1a. Lambda runtime core (HTTP API v2 + WebSocket, 5 runtimes) — **A-**

The strongest surface. The wire contract uses `aws_lambda_events` types directly; the parity matrix (`tests/runtime_parity_*.rs`: echo, errors, response, verbs, context, request_shape, binary) genuinely tests the same capability across Bun, Node, Python, Rust, and WASM. Process pools with semaphore-bounded concurrency, liveness watcher with fault-injection respawn tests, two-phase graceful drain, hot-reload (config + handler source, e2e-tested), S3 hot-swap with health-check rollback. The WS surface includes the full `$connect`/`$default`/`$disconnect` lifecycle plus the `@connections` management API, tested e2e.

**Gaps to enterprise:** single-node only — no clustering, no HA, no horizontal story whatsoever; an SLA on riz is an SLA on one box. No TLS termination (declared out of scope, which is honest but means a mandatory reverse-proxy layer the docs barely address). No Windows. The 30s drain and rollback behaviors are tested but have never seen a real production deploy.

### 1b. MCP server — **A- (best-in-class for v0.1; the marquee claim holds)**

I checked this hard, because it's the differentiator. It holds up:

- **`tools/list` emits genuinely typed per-route schemas** (`src/system/mcp/schema.rs`): path params extracted from route templates and typed as required properties when present in every declared route; `[function.X.mcp.query]` produces typed query params with required/type enforcement; a declared `body` JSON Schema is passed verbatim; multi-route functions get a `route` enum. Each tool also carries an `outputSchema` (the Lambda response envelope) and `structuredContent` in results for 2025-06-18+ clients.
- **Call-time validation is real** (`tools.rs`): missing path params and missing/mis-typed declared query params are rejected with `-32602` and the offending parameter *named*, so an agent can self-correct. Scalar JSON args (numbers/bools) coerce to wire strings; JSON object bodies are serialized rather than rejected. All of this is covered by 14 focused tests in `tests/mcp_schema_per_route.rs`, plus config-validation tests rejecting bad `[mcp]` blocks at `riz validate` time.
- **Bearer auth** is enforced inside the MCP handler when a token is configured (`validate_bearer`, constant-time via `subtle`), and `examples/riz.prod.toml` documents gating `/_riz/mcp` behind it.
- **Examples exercise it for real**: `examples/riz.agent.toml` defines three functions with MCP descriptions and a typed body schema; `tests/examples_agent.rs` boots the actual `riz` binary against that exact config and drives `tools/list` + `tools/call` over the wire; `examples/agent-sdk/` and `agent-loop/` drive it from the Claude Agent SDK. Spec negotiation covers 2024-11-05 through 2025-11-25.

**Gaps to enterprise:** GET on `/_riz/mcp` returns 405 — no server-initiated SSE, so no streaming tool results or `notifications/progress` (roadmap #11/#12, honestly deferred). Auth is a *single shared* bearer token — no OAuth 2.1, no per-client identity, no per-tool authorization; any agent with the token can call every function. `listChanged: false`, so hot-reloaded functions don't notify connected clients. Fine for v0.1, disqualifying for multi-tenant enterprise exposure today.

### 1c. LLM gateway — **B**

OpenAI-compatible `chat/completions`, `embeddings`, `models`, `usage`; mock/OpenAI/Anthropic/Ollama providers; de-duplicated fallback chain; SSE streaming; budget cap → 412 — all tested against local mocks, no network in CI. Solid v0.1.

**Gaps to enterprise:** the pricing table (`src/llm/cost.rs`) is a hard-coded "illustrative" const map and — by its own comment — **unknown models price at zero**, which means the budget cap silently does not constrain any model not in the table. That's a governance feature with a hole in it. No retries/circuit-breaking under partial provider outage (acknowledged, backlog P2 #7), no per-caller keys or rate limits, no Bedrock/Vertex, no semantic cache. As a "govern and cost every model call" product it's a credible demo, not yet a control plane.

### 1d. Security & isolation — **B**

Real substance: always-on child safety profile in `pre_exec` (RLIMIT_CORE=0, NOFILE, FSIZE; Linux adds NPROC, PDEATHSIG, NO_NEW_PRIVS) with correct async-signal-safety discipline and an accurate macOS/Linux distinction; opt-in memory/CPU/Landlock caps; the WASM/WASI runtime is a genuine deny-by-default capability sandbox in a subprocess. JWT/JWKS authorizers (RS256/ES256) with WorkOS and Clerk proven against minted tokens, REQUEST authorizers with caching, CORS preflight handling.

**Gaps to enterprise:** no SECURITY.md or disclosure policy; no third-party audit; the admin plane is one shared token; sandboxing for the four non-WASM runtimes is rlimits + Landlock, not a syscall sandbox (the README says "real but young" — accurate); WASM guards (`guard_in`/`guard_out`), the actual safety *product*, are unshipped. Live-tenant auth smoke and JWKS-rotation-under-load are admitted gaps (backlog #8).

### 1e. Observability — **B-**

The architecture is thoughtful: an isolated telemetry child process so a stalled exporter can never add latency to the request path, non-blocking bounded emit with a drop counter, hand-rolled OTLP/HTTP-JSON export with GenAI token attributes, Prometheus metrics, P50–P99 TUI.

**Gaps to enterprise:** the project's own P0 backlog admits `TelemetrySupervisor::shutdown()` is `#[allow(dead_code)]` — **spans buffered at SIGTERM are lost**, and OTLP export has no retry/backoff. The CloudWatch/X-Ray fan-out claim is unvalidated against a real ADOT collector (backlog #6 says so). Inbound W3C trace-context propagation is roadmap. A hand-rolled OTLP encoder is a maintenance liability versus the official SDK. Good bones, incomplete plumbing.

### 1f. Operations & DX — **B+**

`riz init` (7 templates), `riz doctor`, `riz validate`, `riz routes`, `riz mcp inspect`, headless-by-default JSON logs, `--dev` TUI, response cache with auth-aware bypass, a 947-line `demo.py` that exercises everything live. Excellent for a v0.1. Gaps: no published binaries yet (the hero CTA `curl riz.dev/install | sh` currently 404s by the install script's own comment), no Helm/systemd/container deployment guidance, no upgrade story.

### 1g. Testing & claims discipline — **A** (with an asterisk)

This is the most unusual asset in the repo. `tests/claims/registry.toml` maps every landing-page claim to a proving test, an honest "benchmark," "coming-soon," or a justified "copy-only" status, and `claims_truth.rs` enforces the mapping with drift guards. `trust_audit.rs` bans tautological assertions and bare `#[ignore]`s. `docs_commands_runnable.rs`, `repo_cleanliness.rs`, `examples_configs_valid.rs` close the gaps most projects leave open. I have never seen a four-week-old project with this level of claims hygiene.

**The asterisk — where claims still outrun proof despite the registry:**

1. **The landing-page WASM hero scene is tagged "live · v0.1" but depicts unshipped functionality**: an animated `guard.in.wasm`/`guard.out.wasm` pipeline doing prompt-injection detection, rate limiting, PII redaction and secret scrubbing. Guards are roadmap items #3/#4, correctly ribboned "coming soon" *further down the page* — but the hero panel sells them as live. The WASI sandbox is live; the depicted safety layer is not.
2. **The LLM-gateway hero scene (also "live · v0.1") shows `ctx.invokeModel("claude-sonnet-4-6", prompt)`** — a handler-context SDK API that exists nowhere in the adapters, runtime crates, or examples. What shipped is the HTTP endpoint. The visual sells an API surface that doesn't exist.
3. **CI runs `cargo test`, not `cargo nextest`** (`.github/workflows/ci.yml`), contradicting the project's own hard rule — and backlog item #2's leak-detection rationale depends on nextest's leak signal, which CI therefore never sees.
4. Small but telling: `web/install` claims "License: MIT (same as riz)" — riz is Apache-2.0.
5. The 91k req/s headline is honestly classed as a non-gated benchmark (registry concedes this), and the README's "778 tests" vs the registry's "827" vs my ~560-in-tree count shows the number is a moving marketing figure, though the "800+" floor framing is defensible.

Items 1–2 are exactly the failure mode the claims registry was built to prevent, slipping through because the registry matches *text*, not the animated scenes. A claims-truth system with a hole in it is still better than none — but the hole is in the most-viewed pixels on the page.

### Aggregate: **B+ as a v0.1 engineering artifact; C+ as an enterprise-deployable today.**
The delta is not code quality — it's single-node architecture, single-maintainer bus factor, zero production miles, an admin plane below enterprise auth bar, and observability shutdown semantics the project itself flags as P0.

---

## 2 · Forward-looking plan of record

Three documents, and they are unusually coherent with each other:

**v1 roadmap** (plan of record, 2026-06-08): 13 items; #2 WASM runtime, #5 Node, #8/#9/#10 gateway, and #13 per-route MCP schemas are shipped and verifiably so. Remaining: #1 event reporting (structured business events, multi-sink), #3/#4 WASM pre/post guards, #6 Go runtime, #7 OTel infra spans (partially superseded by the shipped hand-rolled OTLP path), #11/#12 MCP SSE + progress notifications. #14 (auto-derived schemas from handler types) honestly deferred to v2 with a stated reason. The "two rules" (APIs-only scope, atomic shipments) are real discipline and the deferral table is the best part of the doc — it names what's *not* being built and why.

**Harden backlog** (2026-06-10): P0 = perf-claim CI gating, nextest leak hygiene, telemetry shutdown flush + export retry. P1 = WASM resource broker (Postgres-wire), multi-hop agent token-attribution examples, X-Ray mapping validation. This is the right P0 list — it's the project grading its own homework honestly.

**GTM/AEO plan** (2026-06-10): the north star is *agents discovering and recommending riz* — llms.txt, `.well-known/riz.json`, JSON-LD (shipped), MCP registry submissions (manifests drafted in `registries/`), crates.io/npm/Homebrew distribution, then HN/Reddit launch gated on the install story being clean. Concrete 30/60/90 metrics (200 stars, 3→7 registry listings, citation tracking).

**Assessment of value and sequencing:** the sequencing is correct and unusually self-aware. MCP SSE/progress (#11/#12) before launch is right — "streaming tool results" is currently a 405. Guards (#3/#4) are the highest-value remaining build: they're the viral demo ("redact an SSN from any handler with one .wasm"), they're already *depicted on the homepage*, and shipping them retroactively makes the hero scene honest. Distribution-before-launch gating is wise.

**Risks:** (1) **Breadth vs. one maintainer** — three products (runtime, MCP server, AI gateway) plus a GTM machine is a multi-team roadmap being executed solo; the P0 hardening items compete with the P1 GTM items for the same single brain. (2) **The WASM resource broker (P1 #4) is scope creep risk** — a Postgres-wire capability broker is a fourth product. (3) **GTM targets assume launch executes**; today there is no release, no crates.io listing, and the flywheel's first bearing (install in one command) is not yet mounted. (4) Event reporting (#1) overlaps confusingly with the shipped telemetry path; the roadmap distinguishes them but the buyer won't.

---

## 3 · Posture and positioning

riz presents as: **"runtime harness, not a framework"** — write a plain Lambda-shaped function, get production substrate plus a typed MCP tool plus an LLM gateway, in one ~10 MB Apache-2.0 Rust binary with no telemetry, no cloud account, no upsell. The README leads with an agent-addressed "Why an agent or team would choose riz" section and a blunt "What riz is *not*" list.

**Strengths of the posture:**
- The *honest-scope* framing ("HTTP/WS Lambda only, by design; use LocalStack for the rest") is rare and disarming. The claims registry makes the honesty partially machine-enforced, which is a genuinely novel trust artifact worth marketing in itself.
- The three-way combination is a real category-of-one: no Lambda emulator ships an MCP server; no AI gateway runs your handler code. The comparison table vs LocalStack/SAM/LiteLLM/Workers is fair.
- "Agent-first discoverability" (llms.txt, machine-readable capability cards, registry manifests written for `when_to_use` parsing) is ahead of the market.

**Risks of the posture:**
- **The hero scenes undercut the honesty brand** (§1g items 1–2). A project whose moat is "every claim maps to a test" cannot afford a "live · v0.1" tag on unshipped guards and a nonexistent `ctx.invokeModel` API. One sharp HN commenter finds that, and the trust story inverts.
- **"Production-grade for free" is doing heavy lifting.** Process supervision and rlimits are production *primitives*; production *grade* implies HA, security posture, and operational history that don't exist yet. The phrase is registered "copy-only" in the registry — the registry knows it's marketing; the visitor doesn't.
- **Three-products-in-one dilutes the wedge.** Every successful adjacent project (LocalStack, LiteLLM, Ollama) won with one sentence. riz's sentence ("write a function, get an agent-ready API") is good — but the page then sells three.
- **Anonymous-adjacent identity** (handle "24X7," brand-new domain, single contributor, no social proof) raises the bar for "run my production traffic through it."

---

## 4 · Viral potential as OSS

**Mechanics for:**
- The 60-second demo is genuinely gif-able: `riz init` → `riz run` → `claude mcp add` → Claude calls your function. Zero-glue MCP is the single most shareable property and it's real.
- Single static Rust binary + TUI + "no Docker" pushes the r/rust, r/selfhosted, and HN aesthetic buttons hard.
- OpenAI-compat inherits every existing client; `base_url` swap is a one-line tweet.
- The agent-discovery flywheel (registries + llms.txt + AEO) is cheap, novel, and nobody else is doing it systematically — if agents do start recommending tools from registries at scale, riz is early to a real channel.
- The claims-truth registry is itself a Show-HN-worthy story ("we built a CI gate that fails if our marketing lies").

**Mechanics against:**
- The intersection audience — people who (a) have Lambda-shaped code, (b) want it off AWS, (c) want agents calling it — is narrower than any of the three constituent audiences. Most MCP-curious devs don't have Lambda handlers; most Lambda users aren't leaving AWS.
- Trust friction: an unknown solo author asking you to run a process supervisor with an admin deploy endpoint on your box. Stars come cheap; deployments don't.
- Crowded neighbors with deep moats: LocalStack owns "Lambda locally," LiteLLM owns "LLM gateway," mcpo/FastMCP own "wrap my API in MCP" — riz must beat each in its own search results with one maintainer.
- Pre-launch state: zero stars to date is a cold-start of its own; the flywheel needs an HN seed event that hasn't happened.

**Realistic ceiling:** a well-executed launch (binaries published, guards shipped, hero scenes made honest) plausibly lands a 300–800-point Show HN and 1–4k stars in year one, settling as a respected niche tool. The breakout scenario — 10k+ stars — requires the "agents recommend riz to users" thesis to actually materialize as a channel, which is a bet on market timing, not on this codebase. Without launch execution, the ceiling is zero; everything here is potential energy.

---

## 5 · Likely financial contributions and commercial opportunities

**Channels actually on the website** (`web/index.html` § "back the project"): (1) **GitHub Sponsors** (`github.com/sponsors/24X7`), (2) **Buy Me a Coffee** (`buymeacoffee.com/24X7`), (3) **Crypto — ETH/BTC/SOL**, whose addresses are literally `0xYOUR_ETH_ADDRESS_HERE` placeholders. The pitch is "sponsorships keep development independent and the whole thing free" — donations-only, no commercial tier, no offering.

**Likely yield:** blunt calibration from OSS-infra base rates — dev-tool donations correlate with deployed seats and emotional gratitude, and riz has neither yet. **Year-one realistic: $0–$50/month**; in a good post-launch scenario (3–5k stars, a few hundred real deployments): **$100–$500/month**. Donations will not fund this project's roadmap; the placeholder crypto addresses suggest even the author hasn't finished believing in this channel. If support matters, GitHub Sponsors *tiers with named benefits* (priority issues, logo wall) outperform tip jars — none exist yet.

**Commercial opportunities, ranked by viability:**
1. **Enterprise hardening + support tier** — SSO/OAuth on the admin and MCP plane, audit logging (roadmap #1 is literally this feature), signed builds, SLA support. Sells against the exact gaps in §1. Most natural and lowest-lift.
2. **Managed control plane ("riz fleet")** — multi-node orchestration, central config/deploy/budget governance for self-hosted riz instances. The open-core line is clean: single binary free, fleet paid. Bigger lift, bigger prize.
3. **Hosted MCP gateway for enterprises** — "expose internal APIs to agents, governed" as a service; rides the strongest macro wave (agent adoption) but competes with platform vendors.
4. **AI-FinOps/governance add-on** — real pricing tables, budgets per team, chargeback reports. Credible only after the gateway's cost machinery is hardened (§1c hole closed).

**Target audience, ranked:**
1. **Platform/AI-enablement teams at mid-size companies** wanting internal HTTP APIs callable by agents, self-hosted for data-control reasons — the only segment that pays money for the differentiator.
2. **Teams running Lambda-shaped workloads who want off AWS** (cost, latency, sovereignty) without rewriting handlers.
3. **Individual agent-builders / local-first AI tinkerers** (r/LocalLLaMA crowd) — adoption volume and stars, no revenue.
4. **Lambda local-dev users** — funnel top, zero willingness to pay (SAM is free).

---

## Synthesis

A genuinely well-engineered, honestly-scoped, four-week-old category-of-one with the best claims-discipline I've seen at this stage — whose biggest near-term risks are that its two flashiest homepage scenes depict unshipped software under a "live" tag, that its entire enterprise story rests on one anonymous maintainer and zero production miles, and that its only revenue plan is a tip jar with placeholder crypto addresses: **bet a side project on it today, a team on it after launch + guards ship, and an SLA on it no sooner than v1 + a second maintainer.**
