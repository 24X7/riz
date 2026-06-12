# riz — Independent model assessments

Periodic, uncontaminated assessments of riz graded "like a skeptical
platform-engineering lead deciding whether to bet an SLA on it." Each run is
performed by a fresh agent context that is barred from reading prior
assessments; this index (compiled by Claude Opus 4.8) is the only place the
runs are compared.

| Run | Doc | Author | Date | Codebase state |
|---|---|---|---|---|
| 1 | [fable-5-assessment.md](2026-06-12-fable-5-assessment.md) | Fable 5 | 2026-06-12 (am) | pre feature-loop: 831 tests, generic MCP schemas, no SSE/progress, no broker, no guards |
| 2 | [fable-5-assessment-rerun.md](2026-06-12-fable-5-assessment-rerun.md) | Fable 5 | 2026-06-12 (midday) | + typed MCP schemas, example coverage, examples-config guard (~856 tests) |
| 3 | [fable-5-assessment-run3.md](2026-06-12-fable-5-assessment-run3.md) | Fable 5 | 2026-06-12 (pm) | + MCP SSE transport, progress notifications, WASM resource broker (pg), guard_in/guard_out, truth fixes (~900 tests) |
| — | [opus-4.8-assessment.md](2026-06-12-opus-4.8-assessment.md) | Opus 4.8 | 2026-06-12 (am) | companion run-1-era assessment by a different model |

## Grade progression — side by side

Surfaces are bucketed where run boundaries differ slightly (each run chose
its own taxonomy; see the linked docs for each grader's exact scope notes).

| Surface | Run 1 | Run 2 | Run 3 | Movement |
|---|---|---|---|---|
| Lambda HTTP/WS runtime core | B+ | A- | A- | ▲ held |
| MCP server | **B−** | **A-** | **B+** | ▲ (typed schemas, SSE, progress, sessions landed; run 3 docks for buffered-SSE/heartbeat-progress substance — see findings) |
| Isolation / sandboxing / WASM (incl. broker + guards from run 3) | B | B | **A-** | ▲▲ (resource broker + fail-closed guards) |
| LLM gateway | C+ | B | C+ | ◆ flat (fail-closed pricing fixed in run 2; run 3 re-docks for unauthenticated endpoints + in-memory budgets) |
| Observability | B− | B− | B | ▲ |
| Auth | B− | (folded into isolation) | B− | ◆ flat |
| Deploy & lifecycle | B | (not separated) | B+ | ▲ |
| DX / CLI | A− | B+ | A- | ◆ |
| Testing & claims discipline | A− | A | B+ | ◆ (run 3 found the pass-by-skip hole: wasm proofs skip in CI) |
| **Aggregate** | **B** | **B+** (eng) / C+ (enterprise) | **B+** | ▲ |

## Sharpest-findings ledger (and their fates)

| Finding | Found in | Status |
|---|---|---|
| Hero carousel showed unshipped features as "live · v0.1" (guards, semantic cache, `ctx.invokeModel`, Bedrock) | Run 1, re-confirmed run 2 | **Fixed** (1e11700) — and guards subsequently SHIPPED, so the guard pipeline scene returned honestly (b120771) |
| `web/install` claimed MIT; riz is Apache-2.0 | Run 1/2 | **Fixed** (1e11700) |
| CI ran `cargo test` (vs the nextest-only rule) | Run 1/2 | **Fixed** (1e11700) |
| Unknown LLM models priced at $0 — budget cap silently bypassed | Run 2 | **Fixed** fail-closed (1e11700) |
| Crypto donation addresses are literal placeholders | All runs | **Open** — owner action (paste real wallets or drop the card) |
| `/_riz/v1/*` (LLM gateway) + `/cache/invalidate` mounted as bare axum routes — bypass bearer auth; README overclaims gating | **Run 3** | **Fixed** same day — all four gateway routes + cache flush bearer-gated (constant-time, 401-tested incl. wrong-token; ungated local-dev default unchanged); README claim now true |
| Flagship WASM proofs (broker, guards) skip in CI — wasip1 target never installed; claims_truth verifies the proof fn *exists*, not that it *ran* | **Run 3** | **Fixed** same day — CI installs wasm32-wasip1 and builds the broker/guard/echo guest fixtures before nextest, so the keystone e2es RUN in CI. (Deeper guard — claims_truth failing on skipped proofs — remains a backlog nicety) |
| Gateway "SSE streaming" is buffered replay; MCP "progress" is elapsed-time heartbeats, not real progress | **Run 3** | **Open** — calibration: wire-shape is real + tested, substance copy should not overclaim (or upstream token streaming should land) |
