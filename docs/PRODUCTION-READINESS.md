# riz — Production Readiness Roadmap

The bar this document targets: **self-hosted tier-1 infrastructure at a frontier
lab (Anthropic / OpenAI scale)** — the "bet the SLA on it" grade.

## Where we are

The [Power of 10 safety program](assessments/2026-07-03-power-of-10.md) is
complete: 253 baselined violation sites → 0, ten NASA rules adapted to Rust and
enforced across three mechanisms (workspace-deny lints, a CI safety gate, and a
release overflow profile). That made the **code** production-grade at the
function level — no reachable panic/overflow on the request path, bounded queues
with backpressure, fail-closed auth, every `unsafe` justified.

That is the floor, not the ceiling. "Bet your life on it" is a property of the
**system**, and the system has real gaps. This roadmap closes them, ordered so
each phase is independently shippable and truthfully demonstrable.

**Integrity rule for this roadmap:** the website claims only capabilities that
are shipped and tested. The `claims_truth` drift-guard test fails CI if a page
asserts something the suite doesn't back — so every metric below reaches the
site only after its proof exists.

---

## Phase 0 — Isolation is a boundary, not just hardening (highest priority)

Today a worker is confined by `RLIMIT_*` + `prctl(NO_NEW_PRIVS, PDEATHSIG)` +
Landlock (Linux-only, filesystem-only). That is defense-in-depth for trusted
code, not a hard multi-tenant sandbox. A Lambda-style runtime executes code it
does not fully trust.

| Item | Change | Autonomy |
|---|---|---|
| **P0.1 seccomp-BPF allowlist** | Deny-by-default syscall filter applied in `pre_exec`, per-runtime allowlist; a denied syscall kills the child. Child-process test proves enforcement. | ✅ in-repo (`seccompiler`) |
| **P0.2 cgroups v2** | Memory + CPU accounting/limits via a per-function cgroup (replaces `RLIMIT_AS`, which is per-process and broken for JIT runtimes). | ✅ in-repo (`cgroups-rs`) |
| **P0.3 network egress policy** | Per-function egress allowlist; a compromised native worker can currently reach any host. | ✅ in-repo (netns/nftables shell-out) |
| **P0.4 macOS hard-gate** | Off Linux the filesystem sandbox silently no-ops. Prod refuses to enable sandbox-requiring features rather than run without them. | ✅ in-repo |
| **P0.5 microVM option (Firecracker/gVisor)** | The real hard boundary for untrusted tenants; a runtime backend that runs each worker in a microVM. | ⚠️ needs infra + design |
| **P0.6 external sandbox-escape audit** | Third-party pentest of the isolation surface. | ⚠️ needs humans |

Exit criteria: an untrusted worker cannot read outside its allowlist, exhaust
host memory, spawn a fork bomb, make an un-allowlisted syscall, or reach an
un-allowlisted host — proven by tests, on Linux, by default.

## Phase 1 — Close the known security findings (fast, all in-repo)

| Item | Change |
|---|---|
| **P1.1 JWKS authorizer cache** | `JwtAuthorizer` re-fetches JWKS on every cache-missed request — an outbound-amplification lever any invalid token pulls. Cache the authorizer in `AppState`, keyed by `jwks_uri`, with a refresh cooldown. |
| **P1.2 CORS wildcard + credentials** | `allow_origins=["*"] + allow_credentials=true` reflects credentialed origins. Reject the combination in `config validate()` (AWS parity). |
| **P1.3 rate limiting / admission control** | Per-connection queues are bounded; the fleet has no global backstop. Add token-bucket admission + load shedding at the edge. |
| **P1.4 hot_swap concurrency** | Config reload resizes limits but not the concurrency semaphore. Rebuild the pool entry on change. |

## Phase 2 — Distributed systems: escape the single node

Connection store, A2A task store, and LLM budgets are in-process `DashMap`s. A
node restart drops live connections + in-flight tasks; budgets reset.

| Item | Change |
|---|---|
| **P2.1 externalized / durable state** | Move task store + budgets behind a durable store; define an explicit at-least-once / at-most-once contract. Budgets that feed billing cannot stay in-memory. |
| **P2.2 horizontal scale + HA control plane** | Load balancing (WS sticky sessions exist), no single point of failure, leader election where needed. |
| **P2.3 zero-downtime rollout** | Canary + automated rollback; config-validation gate before a swap reaches the fleet. |

## Phase 3 — Operational maturity

| Item | Change | Autonomy |
|---|---|---|
| **P3.1 readiness vs liveness** | `/_riz/health` is a single 200. Split `/_riz/ready` (fails during drain/warmup to gate traffic) from liveness. | ✅ in-repo |
| **P3.2 Prometheus /metrics** | Expose the existing internal counters in Prometheus text format for scraping (OTLP traces already flow). | ✅ in-repo |
| **P3.3 audit log** | Structured, tamper-evident record of deploy/config/access events. | ✅ in-repo |
| **P3.4 SLO/SLI + alerting** | Define latency/availability objectives; wire alerts. | ⚠️ needs infra |
| **P3.5 secrets via KMS/Vault** | Function env/config is not an acceptable secret store; inject via KMS/Vault with rotation. | ⚠️ needs infra |

## Phase 4 — Assurance

| Item | Change | Autonomy |
|---|---|---|
| **P4.1 fuzz the parsers** | `cargo-fuzz` targets for HTTP, JWT, zip, percent-decode, MCP/A2A JSON, and the WASM ABI boundary. | ✅ in-repo |
| **P4.2 load / soak / chaos** | Load at real target scale (the 91k figure is a localhost microbenchmark), memory-stability soak, restart-storm + fault injection. | ⚠️ needs infra |
| **P4.3 supply-chain finish** | cargo-deny + CycloneDX SBOM ship today; add signed releases (cosign), SLSA provenance, reproducible builds, a CVE-patch SLA. | ◑ partial in-repo |
| **P4.4 shrink the TCB** | 6 runtimes + LLM gateway + A2A mesh + MCP + broker in one binary is a large trusted base. Split the gateway and broker into separately-deployed, separately-audited services. | ⚠️ needs design |

---

## What software alone cannot close

Some of "bet your life on it" is not code: an external security audit, an
on-call rotation with runbooks and incident response, a load profile from real
traffic, and the infra decisions behind microVM isolation, HA, and secret
management. Those are marked ⚠️ above and are owned by humans + platform, not by
this repo. The ✅ items are what ships autonomously and truthfully first.

## Sequence

**P0 (isolation) → P1 (known findings) → P3 ops (✅ items) → P4.1 fuzz →
P2 (distributed) → the ⚠️ infra/human items.** Isolation first because it is the
single largest gap between "well-built v0.1" and "a frontier lab would run it."
