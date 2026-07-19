# Shape-purity Tranche 3 (PR4 — guest ABI v2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the per-verb guest ABI to one dispatcher import — `riz_capability.call(verb, grant, req)` — so R2's capability suite grows by verb strings, never new imports. riz-wasm 0.2; sanctioned pre-1.0 break with an actionable load-time error for stale binaries.

**Architecture:** Host-side, `riz_broker.{pg_query,read_response}` becomes `riz_capability.{call,read_response}`; `call` gains `(verb_ptr, verb_len)` and dispatches on the verb string (`"pg.query"` → the existing `Broker::pg_query`; anything else → the closed-set `bad_request` envelope). The stash protocol, `guest_range` bounds checks, and deny-by-default no-grants envelope are unchanged. riz-wasm's `cap::pg::query` targets the new import with verb `"pg.query"`; author API unchanged.

**Tech Stack:** wasmtime 45 linker funcs, riz-wasm 0.2, WAT-based negative test.

## Global Constraints

- Same gates as prior tranches; merge via `--squash --admin`.
- src/ under docs/SAFETY.md: no unwrap/indexing, bounded everything; follow the existing `add_broker_imports` patterns exactly.
- Wire contract at the HTTP boundary untouched. The capability ABI break is sanctioned (greenfield rule); the failure mode for an old binary must be actionable, not a bare linker error.
- PR5 (broker→daemon + sqlx pool) gets its own tranche-4 plan after this lands — its src/ surface is too large to co-plan blind.

---

### Task 1: host — `riz_capability` dispatcher import

**Files:** Modify `src/process/wasm.rs` (`add_broker_imports` → `add_capability_imports`; instantiate error hint; unit tests + WAT negative test).

**Interfaces (produced, consumed by riz-wasm 0.2 + fixtures):**
- Import module `riz_capability`:
  - `call(verb_ptr, verb_len, grant_ptr, grant_len, req_ptr, req_len) -> i32` — stashes the JSON envelope, returns its length; −1 on ABI fault. No grants armed → `denied` envelope. Unknown verb → `{"ok":false,"error":{"code":"bad_request","message":"unknown capability verb …"}}`.
  - `read_response(dst_ptr, dst_cap) -> i32` — unchanged semantics.
- Instantiating a module that still imports `riz_broker.*` fails with an error containing `rebuild against riz-wasm >= 0.2`.

- [ ] Step 1: rewrite `add_broker_imports` → `add_capability_imports` with the six-arg `call` (verb read + bounds-checked like grant/req; dispatch match on the verb string; only `"pg.query"` wired). Delete the `riz_broker` registrations.
- [ ] Step 2: wrap the `linker.instantiate` error: if its display contains `riz_broker`, append `— this module was built against the pre-0.2 riz-wasm capability ABI; rebuild against riz-wasm >= 0.2`.
- [ ] Step 3: unit tests — verb dispatch to bad_request for unknown verbs (pure helper), plus a WAT module importing `riz_broker.pg_query` instantiated against the new linker asserting the actionable message.
- [ ] Step 4: `cargo nextest run --bin riz -E 'test(wasm)'` green; commit.

### Task 2: riz-wasm 0.2 + fixture

**Files:** Modify `crates/riz-wasm/Cargo.toml` (0.2.0), `crates/riz-wasm/src/lib.rs` (cap::pg extern block → `riz_capability.call` with verb `"pg.query"`; docs), `tests/fixtures/broker-wasm/src/main.rs` (raw externs → `riz_capability.call`, keeping the −1/length-mismatch adversarial paths), `tests/wasm_broker_pg.rs` (comment/protocol refs only if needed).

- [ ] Step 1: update shim + bump version; unit tests unchanged (decode_response untouched).
- [ ] Step 2: update broker-wasm raw externs (verb passed explicitly — the fixture stays the adversarial raw-ABI coverage).
- [ ] Step 3: rebuild guests + fixtures for wasm32-wasip1; `cargo nextest run --test wasm_broker_pg --test wasm_examples --test wasm_guards --test runtime_parity_request_shape` green; commit.

### Task 3: gates + ship

- [ ] Full gate suite (workspace + both smokes — template_smoke_all's wasm leg builds against the 0.2 shim via path patch, proving the new ABI end-to-end); push; PR "feat(wasm-abi): single riz_capability.call dispatcher — guest ABI v2, riz-wasm 0.2 (PR4)"; merge `--admin`; pull main.

## Self-Review
Covers the full PR4 spec text: dispatcher collapse, pg_query deletion, riz-wasm 0.2 lockstep, actionable stale-import failure, fixtures updated in-PR, author API unchanged. Names consistent across tasks (`riz_capability`, `call`, `"pg.query"`). No placeholders — the host signature, envelopes, and error string are stated exactly.
