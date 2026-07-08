# Design: drift remediation, supply-chain guardrails, and a security-posture page

**Date:** 2026-07-08
**Status:** approved for spec review
**Origin:** independent assessment of an external (codex) review of riz. Only the
claims that verified against the actual code/site are in scope; the review's
opinion/positioning items and its refuted claims are excluded (see "Excluded").

## Goal

Three independently-shippable packages, in order:

- **A — drift fixes:** remove stale counts/comments/CLI text that contradict the
  code. Protects the project's "every claim maps to a passing test" posture.
- **B — supply-chain guardrails:** add the CI/release machinery (cargo-deny, SBOM,
  build provenance) so the security page can reference real facts, not intentions.
- **C — security-posture page:** a `web/security.html` that states what each
  isolation layer does and does not protect — the boundary counterpart to the
  existing `web/sandbox.html` marketing page.

A depends on nothing. C depends on B (it references B's outputs as shipped facts).

---

## Package A — Documentation & comment drift fixes

All edits verified against source during assessment. `src/` edits are subject to
the SAFETY.md lint tier; removing a now-unneeded `#[allow]` is a promotion in the
right direction, but each removal must be confirmed by `cargo clippy -D warnings`
(the symbol must actually be used now, or the build breaks).

| # | File:line | Current | Change |
|---|---|---|---|
| A1 | `Cargo.toml:50` | `…five runtimes (Bun, Node.js, Python, Rust, WASM)…` | six; add Go |
| A2 | `CLAUDE.md:4` | `five runtimes (Bun, Node.js, Python, Rust, WASM)` | six; add Go |
| A3 | `registries/README.md:31` | `five runtimes in one binary` | six |
| A4 | `src/config.rs:774` | `RuntimeKind::Go` doc comment references `crates/riz-go-runtime` (no such dir) | point at the real artifacts (`examples/lambdas/echo-go`, `templates/go-http`) or drop the path |
| A5 | `src/llm/mod.rs:53-54` | `…real HTTP providers (OpenAI/Anthropic/Ollama) land in follow-up commits.` | they've landed; describe present state |
| A6 | `src/llm/mod.rs:165-167` | `Real HTTP providers … are added in follow-up commits;` | same |
| A7 | `src/llm/types.rs:16` | `// … forwarded to the real providers (follow-up commits).` | drop the `(follow-up commits)` |
| A8 | `src/config.rs:167` | `/// Consumed by the real HTTP providers (follow-up commits).` | drop the `(follow-up commits)` |
| A9 | `src/llm/mod.rs:565` | `/// No native stream for this provider (mock; anthropic for now)…` | Anthropic now streams (translated to OpenAI chunks); correct the parenthetical |
| A10 | `src/llm/mod.rs:64-65` | `// Used for log/introspection once multiple provider kinds ship.` + `#[allow(dead_code)]` | multiple kinds already ship; update comment; drop `#[allow(dead_code)]` **iff** clippy confirms it's now used |
| A11 | `src/gateway.rs:7-8` + `:11` | `// WebSocket types are consumed only from integration tests … until WS Task 7 (upgrade handler) lands.` + `#[allow(unused_imports)]` | upgrade handler landed (`src/ws/upgrade.rs`), library code uses these types; correct comment; drop `#[allow(unused_imports)]` **iff** clippy confirms |
| A12 | `src/main.rs:351` | `git init + initial commit (use --git to disable)` | `--git` *enables* (opt-in); there is no `--no-git`. Drop the misleading parenthetical (the line only prints when `--git` was passed) |

**Non-goal for A:** do NOT add a `--no-git` flag. A12 is a copy fix, not a
behavior change; the flag stays opt-in `--git`.

**Verification (A):**
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # catches A10/A11 allow removals
cargo nextest run --workspace -E 'not binary(e2e_smoke_all)'
```
Grep sweep afterward: `rg -n "five runtimes|follow-up commits|crates/riz-go-runtime|to disable"`
returns only intended survivors (dated docs under `docs/assessments/**` and
`docs/superpowers/plans/**` are historical and out of scope).

---

## Package B — Supply-chain guardrails

Current state (verified): no `cargo-deny`, no `cargo audit`, no SBOM, no signing,
no `deny.toml`. CI (`.github/workflows/ci.yml`) runs fmt, clippy, safety gate,
build, WASM/Go fixtures, nextest, e2e smoke. Release (`.github/workflows/release.yml`)
builds cross-platform binaries.

### B1 — `deny.toml` + cargo-deny CI gate
- Add `deny.toml` at repo root configuring four checks: `advisories` (RUSTSEC),
  `licenses` (allowlist matching the current dependency tree — Apache-2.0/MIT/BSD
  family), `bans` (deny duplicates/yanked where clean), `sources`.
- Add a `cargo-deny` job to `ci.yml` (via `EmbarkStudios/cargo-deny-action`).
  Gated (fails CI) once the tree is clean.
- **Risk / discovery:** cargo-deny may surface a real existing advisory or a
  license the tree already depends on. Handling, in priority order: (1) update the
  offending dependency; (2) if no fix exists, add a **documented, dated** ignore in
  `deny.toml` with the RUSTSEC id and rationale. Either way the CI stays green and
  the state is recorded — this is expected discovery, not a blocker.

### B2 — CycloneDX SBOM on release
- Generate a CycloneDX SBOM in `release.yml` (e.g. `cargo-cyclonedx`) and attach it
  as a release asset (`riz.cdx.json`). One step; no gating.

### B3 — Build-provenance attestation
- Add `actions/attest-build-provenance` for the release binaries in `release.yml`
  (keyless; needs `id-token: write` + `attestations: write` on the job). No secrets.
- Verified by consumers with `gh attestation verify <file> --repo 24X7/riz`.

**Deferred to roadmap (documented on the C page, NOT built here):**
- Detached cosign signatures.
- `curl … | sh` installer verifying signature/checksum before executing (touches
  the outward-facing hosted install path — larger, riskier, its own effort).
- External security audit.

**Verification (B):**
- `cargo deny check` passes locally on the current tree (after handling any B1
  discovery).
- SBOM step produces a valid `riz.cdx.json` (validate structure).
- Release workflow YAML parses; provenance step only fires on release, so confirm
  by workflow lint + a dry `workflow_dispatch`/tag where practical.

---

## Package C — `web/security.html`

A new site page in the established style (same `<nav>`, `<footer>`, `site.css`,
`llms.txt` alternate link, JSON-LD `SoftwareApplication`, `.well-known` reference).
It is the boundary/threat-model counterpart to `web/sandbox.html`: `sandbox.html`
sells the capability; `security.html` states the perimeter precisely, including
where isolation stops. Every protection statement links a real test or code path,
matching the site's claims-pinned convention.

### Sections
1. **Hero** — frames the page as the perimeter statement: what riz confines, and
   where that confinement ends. No swagger; concrete.
2. **Isolation matrix** — a table keyed by runtime class:
   - **Native (Bun / Node.js / Python / Rust / Go):** confined by the OS process
     boundary + always-on profile — `RLIMIT_CORE=0`, `RLIMIT_NOFILE=4096`,
     `RLIMIT_FSIZE=100 MiB`, Linux `RLIMIT_NPROC=4096`, `PR_SET_PDEATHSIG(SIGKILL)`,
     `PR_SET_NO_NEW_PRIVS`; opt-in `RLIMIT_AS`/`RLIMIT_CPU`; and on **Linux ≥5.13**
     a Landlock filesystem allowlist. **Not confined:** arbitrary syscalls (no
     seccomp), network egress unless you restrict it externally, and on
     **macOS/BSD there is no Landlock** — only the rlimits + (Linux-only) prctl
     pair, so those platforms get DoS bounds, not filesystem confinement.
   - **WASM (`runtime = "wasm"`):** WASI deny-by-default filesystem + network,
     host-side capability broker for data access (credentials never cross the WASI
     boundary), and fail-closed `guard_in`/`guard_out`. This is the strong-isolation
     path and the recommended one for untrusted / model-generated code.
3. **Three walls, with their limits** — cross-link `sandbox.html`, then state the
   boundaries plainly: Landlock requires kernel ≥5.13 and degrades best-effort on
   older kernels; rlimits are resource/DoS ceilings, not a confinement boundary;
   there is no built-in user-namespace/container isolation — compose your own
   (container, VM, dedicated user) for defense in depth.
4. **Threat model** — assets (host, co-tenant data, credentials, the network),
   trust boundaries (the WASI boundary, the OS process boundary, the auth surface
   on `/deploy` and `/_riz/*`), and in/out-of-scope, reusing `SECURITY.md`'s scope
   section. Note the fail-closed `/deploy` gate (503 with neither key nor CIDR).
5. **Supply chain** — references B as shipped: cargo-deny advisory/license/bans
   gate, CycloneDX SBOM per release, build-provenance attestation with the
   `gh attestation verify` invocation. Roadmap subsection: cosign detached
   signatures, verify-on-install, external audit.
6. **Reporting** — links `SECURITY.md` (private advisory / chris@riz.dev, 3-day ack).

### Wiring
- Add `security.html` to the nav on every page (and the footer "Project"/"Product"
  group), cross-link from `sandbox.html`.
- Add it to `web/llms.txt` and, if that file enumerates pages, `.well-known/riz.json`.
- `SECURITY.md` links to the posture page.
- If site page-claims are registered in `tests/claims/registry.toml`, register the
  security page's concrete claims there, tagging copy-only vs test-backed per the
  existing status convention (mirror how `sandbox.html`'s proof names are handled).

**Verification (C):**
- Every protection claim on the page maps to a named test or `src/` line
  (e.g. `child_inherits_always_on_caps`,
  `child_with_allowlist_can_read_allowed_path_only`, `src/process/safety.rs`,
  `src/deploy.rs`). No claim without a referent.
- Page renders and reads correctly (dark-contrast check per prior feedback);
  nav/footer links resolve; `llms.txt`/`.well-known` stay consistent.
- No banned framing ("honest"/"truthful"/"candid") in the copy.

---

## Excluded (verified as not-actionable)

- **Homepage crawlability** — refuted. `web/index.html` is ~24 KB of static HTML
  (JSON-LD, `llms.txt`, `.well-known/riz.json`); a live fetch of `https://riz.dev/`
  returned the full hero, pitch, runtime list, and install command. The reviewer's
  "Loading…" observation is not reproducible in source or on the deployed site.
- **In-memory LLM budget ledger** (`Mutex<HashMap>`), **per-instance WebSocket
  state / no cross-replica broadcast** — both TRUE but by design for a single-binary
  runtime and already documented (`docs/CAPABILITY-CARD.md:40`). Not defects.
- **`/deploy` fail-closed** — TRUE and already a positive; referenced by C, not changed.
- The review's larger content asks (production deployment guide, benchmark
  methodology page, homepage message rewrite) — legitimate but out of this scope by
  the user's selection.

## Sequencing

Three PRs: **A** (independent, ship first) → **B** → **C** (references B). Each
passes the full build/test gates in CLAUDE.md before merge.
