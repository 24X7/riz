# Independent Assessment of riz — Run 3

**Author:** Claude Fable 5 (run 3)
**Date:** 2026-06-12
**Scope:** Full independent assessment of the riz OSS project at v0.1, branch `claims-truth-ai-substrate`: feature surfaces graded for production/enterprise readiness, plan of record, OSS posture/positioning, viral potential, and financial/commercial outlook.
**Basis (what I actually inspected, primary sources only):** `Cargo.toml`, `README.md`, full reads of `src/system/mcp/{schema,transport,tools,protocol}.rs`, `src/broker/{mod,pg}.rs`, `src/process/{guard,wasm,safety}.rs` (+ guard wiring in `src/process/mod.rs` and `src/runtime/process.rs`), `src/observability/otel.rs`, `src/server.rs` (route mounting + auth paths), `src/auth/*` (sizes + bearer/authorizer call sites), `src/llm/*` and `src/system/openai_compat.rs` (mounting + streaming path), `src/deploy.rs` (fn survey), tests `wasm_guards.rs`, `wasm_broker_pg.rs`, `claims_truth.rs`, `tests/claims/registry.toml`, the tests/ directory listing (~70 integration files; ~598 test fns counted by grep across tests+src; the suite reportedly lists ~900 via nextest — not re-run here to avoid build artifacts), `.github/workflows/ci.yml`, `web/index.html` (hero scenes + support section), `docs/plans/{v1-roadmap, 2026-06-10-gtm-and-aeo, 2026-06-10-harden-backlog}.md`, `examples/`, and `git log`. Per instruction, nothing under `docs/assessments/` was read.

---

## 1. Features, graded for production/enterprise-grade nature

Grading stance: would a skeptical platform-engineering lead bet an SLA on this surface today?

### Lambda HTTP/WS runtime core — **A-**
The strongest surface. Real `aws_lambda_events` HTTP API v2 + WebSocket wire contract, one warm process pool per function, semaphore-bounded concurrency, liveness watcher with fault-injection-tested respawn (<250ms), two-phase graceful shutdown, hot-reload of config and handler source, and a cross-runtime parity matrix (verbs, params, cookies, binary bodies, errors, context) across Bun/Node/Python/Rust/WASM that is the kind of testing most runtimes never do. `kill_on_drop`, pipe drop-guards, and clock-skew guards show someone has chased real race conditions.
**Gaps to enterprise:** single-node only — no HA, no clustering, no shared WS connection store; no TLS termination (deliberate, but an SLA needs a fronting proxy); macOS gets rlimits but not Landlock/pdeathsig; the 91k req/s number is a router microbenchmark, labeled as such but not CI-gated (acknowledged P0 in the harden backlog); nextest "leaky" tests acknowledged but unresolved.

### MCP server — **B+**
Genuinely spec-literate: JSON-RPC 2.0, version negotiation (2024-11-05 → 2025-11-25 default), Streamable HTTP with POST-SSE response frames, GET server channel with keepalives, DELETE session termination, `Mcp-Session-Id` issued on initialize, 202 for notifications. `tools/list` emits **real typed schemas**: path params typed from route templates with correct required-only-when-on-every-route logic, `[function.x.mcp]` query/body typing, call-time validation with agent-correctable error messages, plus `outputSchema` (the Lambda response envelope) and `structuredContent` for 2025-06-18+ clients. This is better MCP engineering than most dedicated MCP servers.
**Gaps:** (1) Progress notifications are **elapsed-time heartbeats**, not real progress — a 500ms ticker emitting "tool call running (Xs elapsed)". Spec-conformant and useful for keep-alive, but the marketing word "progress" flatters it. (2) Sessions are correlation-only — stateless, no resumability/redelivery (`Last-Event-ID` not supported). (3) Auth is a single shared static bearer token for the whole tool surface — no OAuth 2.1 (deferred to v2), no per-tool scoping, no per-client identity. For an enterprise exposing product APIs to third-party agents, that's the first blocker.

### WASM isolation: sandbox + resource broker + guards — **A-** (design), with a verification asterisk
This is the differentiated engineering in the repo, and it's real:
- **Sandbox:** `wasm32-wasip1` under wasmtime 45 inside a separate `riz __wasm-host` OS process — two boundaries, not one. Deny-by-default: stdio only; `allowed_paths` → preopens; `stage_variables` → guest env; host env not inherited.
- **Broker (can a guest reach Postgres?):** Yes — and the control design is textbook. The guest never opens a socket and never sees a DSN; it calls a `riz_broker.pg_query` host import against a *named grant*. The single dispatcher seam enforces, in order: deny-by-default grant lookup, request payload cap, token-bucket rate limit, concurrency cap that **rejects rather than queues**, per-call deadline, response payload cap, and a per-call audit log line. The PG backend layers server-side `statement_timeout` behind the broker deadline, and read-only grants run inside `READ ONLY` transactions — writes refused by Postgres itself, not SQL inspection. Credentials resolve host-side from env and never serialize toward the guest. The e2e test proves grant/deny/stall-bounded against a real PG-wire mock across the real process tree.
- **Guards (guard_in/guard_out):** A `.wasm` policy module rides the same pool machinery as handlers; verdict contract is allow / allow+mutate / deny(status,body). **Fail-closed is engineered, not asserted**: `GuardVerdict::default()` is deliberately not a valid verdict, so an unhealthy/crashed/garbage guard parses as "not understood" → 502; a guard that can't spawn is a startup error; guards get a hard 2s non-configurable budget. The e2e test covers allow/deny/mutate/garbage→502, cross-runtime (same module wrapping Bun and WASM), and SSN redaction in guard_out.
**The asterisk:** CI (`ci.yml`) never installs the `wasm32-wasip1` target or builds the guard/broker/echo wasm fixtures, and those e2e tests **skip cleanly when artifacts are missing** — so the keystone WASM proofs likely pass-by-skip in CI and are only truly executed on a dev machine. **Other gaps:** broker is PG-only (one verb); one mutex-serialized PG connection per worker (fine under the concurrency cap, not a pool); a read-write grant still allows arbitrary SQL from a compromised guest within its limits; WASI Preview 1 only.

### LLM gateway — **C+**
The shapes are right: OpenAI-compatible `/_riz/v1/{chat/completions,embeddings,models,usage}`, four providers (mock/OpenAI/Anthropic/Ollama), model-prefix routing, de-duplicated fallback chain, budget cap → 412, per-provider cost telemetry, token attrs flowing into OTLP spans. Two material problems:
1. **The `/_riz/v1/*` routes are mounted as bare axum routes that bypass the bearer-auth path entirely.** `dispatch_lambda` enforces `RIZ_AUTH_BEARER_TOKEN` for `/_riz/*` system functions (metrics has a 401 test), but the gateway endpoints are `.route()`-mounted before the fallback and their handlers do no auth; no test asserts a 401 on `/_riz/v1`. The README claim "`RIZ_AUTH_BEARER_TOKEN` gates `/_riz/*`" is false for the one endpoint that **spends your provider API budget**. `/cache/invalidate` is similarly unauthenticated. On a self-hosted prod box this is an unauthenticated money-spender and cache-flusher.
2. **Streaming is buffered re-emission, not token streaming.** `stream_response` replays a *completed* response as SSE chunks; the code comment admits "Real providers will proxy upstream token streams when they land." The page's "SSE streaming" claim is wire-shape true and substance hollow.
Also missing for enterprise: per-caller keys/quotas, retries/circuit breaking (acknowledged P2), Bedrock/Vertex (ribboned as coming-soon).

### Observability — **B**
Prometheus `/_riz/metrics`, rich health, P50–P99 TUI, and a hand-rolled OTLP/HTTP-JSON exporter (no OTel crate tree — defensible for binary size) running in an isolated telemetry child, with bounded retry/backoff (3 attempts, classified transient/permanent), graceful shutdown flush, and GenAI token attributes with multi-hop rollup tests. **Gaps:** traces only — no OTLP metrics/logs signals; no W3C `traceparent` propagation inbound/outbound (roadmap); X-Ray segment mapping unvalidated against a real collector (acknowledged); the hand-rolled encoder is a standing maintenance liability as OTLP evolves; structured business-event reporting (roadmap #1) unshipped — that's the audit-log surface enterprises actually ask for first.

### Auth — **B-**
Handler-side is solid: REQUEST authorizers (call a user function, TTL cache) and JWT/JWKS (RS256/ES256) with WorkOS and Clerk proven end-to-end against minted-key fixtures, including Clerk's no-`aud` default token; constant-time bearer compare via `subtle`. **Gaps:** the admin plane is one static bearer token; the gateway/cache holes above are auth-surface failures; no OAuth 2.1 on MCP; live-tenant and JWKS-rotation coverage acknowledged as nightly-job debt; no mTLS story.

### Deploy / lifecycle — **B+**
S3 hot-swap with 30s in-flight drain, post-swap health check with automatic rollback (422-on-crash tested), unique staging dirs, **symlink-skipping zip unpack** (zip-slip defense, regression-gated), deploy endpoint refuses when no auth is configured, CIDR allowlisting. Good single-box ops. **Gaps:** single artifact source (S3), single-node atomicity only (no fleet rollout), fixed drain window, no deploy history/audit endpoint, no signed artifacts.

### DX / CLI — **A-**
`riz init` (7 templates, 4 languages), `doctor`, `routes`, `validate`, `mcp inspect` (a genuinely good idea — one-screen initialize+tools/list with schemas), `--dev` TUI, headless JSON logs by default, hot reload, one ~35 MB binary, a `demo.py` that exercises everything, runnable-docs tests, and a clean repo-cleanliness gate. **Gaps:** no published release binaries yet (README admits), not on crates.io/npm/Homebrew yet — the landing page's `curl riz.dev/install | sh` is ahead of distribution reality; no `wasm-http` template yet.

### Testing / claims discipline — **B+**
The claims registry is the most distinctive process artifact here: every landing-page capability claim is mapped to proven/benchmark/coming-soon/copy-only, with verbatim page-text drift guards, enforced by `claims_truth.rs` (orphan ribbons fail, roadmap items can't masquerade as shipped, copy-only requires a self-justifying note). The copy-only notes concede more than the marketing they govern. **Gaps that matter:** (1) "proven" verifies the proof **function exists in source**, not that it ran or passed — combined with the CI wasm-fixture gap, the flagship WASM claims are enforced by tests that skip in CI; (2) test-count drift: README says 778, page says 800+, registry note says 827, current suite reportedly ~900 — the page floor holds, the README is stale; (3) perf claim not CI-gated (acknowledged P0).

### Aggregate: **B+**
Far above a typical v0.1 — the runtime core, WASM stack, and truth discipline are genuinely strong — but "enterprise-grade" requires closing the unauthenticated gateway/cache endpoints, executing (not just possessing) the WASM proofs in CI, real token streaming, OAuth-grade MCP auth, and a multi-node story. Today it is an excellent **single-box** substrate.

---

## 2. Forward-looking plan of record

**v1 roadmap (13 items):** ~11 shipped by commit history (WASM runtime, Node, gateway trio, MCP SSE/progress/typed schemas, guards). Remaining: **#1 event reporting** (the highest-enterprise-value unshipped item — audit-grade per-invocation events), **#6 Go runtime** (real funnel-widener; Go is the #2–3 Lambda runtime), and trace-context propagation. The roadmap's discipline is unusual: explicit out-of-scope tables, YAGNI rationale for rejecting WS-as-MCP-transport, and #14 (auto-derived schemas) deferred with a stated cost argument.

**Harden backlog:** correctly ranked. P0s — perf-claim CI gating, leak hygiene, telemetry shutdown (the last shipped per git log). P1 broker + agent-loop examples shipped. The backlog itself documents the gap between claim and CI gate, which is to its credit.

**GTM/AEO plan:** a coherent, novel thesis — optimize for *agents* discovering and recommending riz (llms.txt, .well-known manifest, MCP registries, decision-oriented posts), sequenced registries → AEO → packages → HN. Concrete 30/60/90 metrics (200 stars, 3→7 registry listings, citation tracking).

**Risks:** (a) the AEO flywheel is an unproven channel — agent recommendation behavior is not yet a measurable acquisition motion, and the citation targets are hope-shaped; (b) **distribution debt blocks everything** — no crates.io, no npm, no binaries, while the landing page already advertises an install one-liner; the plan knows this ("credibility gate") but it's still unshipped; (c) single-maintainer bus factor across runtime + gateway + MCP + GTM; (d) MCP-registry fit is awkward — riz is a *runtime that exposes dynamic tools*, not an installable MCP server, and registries may not have a slot for that (the manifests don't hide this, but rejection risk is real); (e) the GTM doc still says "~10 MB binary" — stale vs the actual ~35 MB elsewhere.

---

## 3. Posture and positioning

**Strengths.** The positioning is sharp and true: "runtime harness, not a framework," scope stated up front (HTTP/WS only, not an AWS emulator), an explicit "What riz is *not*" section, and a "Skip riz when…" list — rare candor. The three-products-one-binary combination (Lambda runtime + MCP server + LLM gateway) is a genuine category-of-one; the competitor table holds up against LocalStack/SAM/LiteLLM. The claims-truth machinery makes the marketing *auditable*, which is itself a positioning asset.

**Hero scenes vs shipped reality (checked):** all four "live · v0.1" scenes correspond to shipped, tested code. Scene 1 (MCP typed schemas) — true, proven by `mcp_schema_per_route.rs`. Scene 2 (WASM guards pipeline, fail-closed, broker) — true, proven by `wasm_guards.rs`/`wasm_broker_pg.rs` *when artifacts exist* (see CI caveat). Scene 3 (gateway budgets/fallback) — true except "SSE streaming · live," which is buffered replay, the one scene line that outruns substance. Scene 4 (TUI) — shipped. The 91k req/s headline links to methodology. Net: claims-to-reality fidelity is unusually high, with two soft spots (streaming substance; "progress" being heartbeats).

**Risks.** Zero users and zero distribution at the moment the page reads like a launched product; the unauthenticated `/_riz/v1` surface contradicts the README's own production notes and would be a credibility wound if found by an HN commenter before the maintainer fixes it; the **support section ships placeholder crypto addresses** (`0xYOUR_ETH_ADDRESS_HERE`) — live proof the funding plumbing has never been exercised; breadth-vs-depth — five runtimes, three product surfaces, one maintainer is a sustaining-velocity risk; and the category-of-one framing cuts both ways: no category means no search demand, which is exactly why the plan leans on AEO.

---

## 4. Viral potential as OSS

**For:** the 60-second demo is real and excellent (`riz init` → `riz run` → `claude mcp add` → agent calls your function) — "your API is an agent tool with zero glue" is a top-tier HN hook in 2026; single Rust binary + TUI + WASM sandbox hits three communities (r/rust, r/selfhosted, r/LocalLLaMA) with distinct angles; the claims-truth registry is itself a Show-HN-worthy story ("our marketing page is CI-tested against our test suite"); the WASM guard demo ("redact an SSN from any handler with one .wasm") is genuinely novel.

**Against:** no installable artifact today — virality dies at `cargo install --git`; the audience intersection ("runs Lambda-shaped code" ∩ "self-hosts" ∩ "wants agents") is narrower than any of its parts; LiteLLM/LocalStack own adjacent mindshare with massive head starts; single-maintainer projects plateau when issues outpace one person; MCP enthusiasm could consolidate into platform-native tooling (Claude/Cursor shipping their own glue), eroding the zero-glue differentiator.

**Realistic ceiling:** a well-executed launch (binaries + crates.io + demo GIF first) plausibly lands 500–2,000 stars in the first quarter; 5k within a year if the agent-tooling wave keeps building and the maintainer ships the Go runtime + event reporting. 20k-star LiteLLM-class outcomes would require either a viral moment or a second act (hosted offering, marquee adopter). The tight-scope culture caps downside more than it raises the ceiling.

---

## 5. Likely financial contributions and commercial outlook

**Channels on the website** (`web/index.html` #support): GitHub Sponsors (`github.com/sponsors/24X7`), Buy Me a Coffee (`buymeacoffee.com/24X7`), and crypto (ETH/BTC/SOL) — whose addresses are **unfilled placeholders**, so that channel currently collects nothing by construction. The pitch ("free, local-first, independent; sponsorships keep it free") is donation-framed with no commercial tier, no support contract, no hosted offering.

**Realistic donation forecast:** infrastructure tools monetize via companies, not tips. Pre-launch with zero users: ~$0. Post-successful-launch (1–2k stars): GitHub Sponsors for solo infra projects of this profile typically lands **$0–100/month**, occasionally a few hundred with a viral moment. Buy Me a Coffee adds noise-level one-offs. Over the first year, expect **low hundreds of dollars total** through the articulated channels. These channels are signaling (the Sponsor button as social proof), not income.

**Commercial opportunities, ranked by viability:**
1. **Enterprise support/SLA contracts** for self-hosted riz (security review, upgrade guarantees, priority fixes) — the natural first dollar; needs ~5 production adopters first.
2. **"riz fleet" control plane** (open-core): multi-node deploys, centralized config, shared WS state, fleet observability, SSO/audit — exactly the enterprise gaps cataloged in §1, which is a coherent open-core seam (single box free forever; fleet paid).
3. **Hosted riz** (managed agent-API substrate: bring a function, get a governed MCP endpoint + gateway with billing) — biggest TAM, biggest lift, competes with platform clouds.
4. **Guard/policy marketplace or certified policy packs** (PII, prompt-injection, compliance rule-sets as `.wasm`) — novel, rides the differentiator, unproven market.
5. **Paid gateway tier** (semantic cache, eval harness, prompt versioning per v2 roadmap) — viable but contested by LiteLLM/Portkey/Helicone.

**Target audience, ranked:** (1) platform/AI-infra engineers at agent-adopting companies who need existing internal APIs exposed as governed agent tools — the buyer the whole thesis points at; (2) teams running Lambda-shaped workloads off-AWS (cost exits, on-prem/regulated, CI) — the Trojan-horse entry; (3) security-conscious teams executing untrusted/LLM-generated code who want the WASM sandbox + broker — small today, the fastest-growing segment; (4) Rust/self-hosting enthusiasts — stars and PRs, not revenue; (5) indie/agent-builder hobbyists via Ollama + local MCP — community mass, zero dollars.

**Viability verdict:** as a donation-funded OSS project, not viable beyond hobby economics. As the open-core seed of an agent-API-substrate company, the engineering quality, scope discipline, and claims-truth culture are exactly the right foundation — but it needs distribution shipped, the gateway auth hole closed, and its first ten production users before any commercial motion is real.

---

**One-line synthesis:** riz is an unusually well-engineered single-box agent-API substrate — A-grade runtime and WASM-capability work undercut by a B-/C-grade seam (unauthenticated gateway endpoints, WASM proofs that skip in CI, faux streaming) and zero distribution, making it a strong open-core bet and a weak donation business until the maintainer ships binaries and closes the gap between its claims machine and its CI.
