# Shape-purity Tranche 1 (PR0 + PR1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land PR0 (drift fixes + spec as source of record) and PR1 (riz-wasm shim on the current wire, echo-wasm/orders-wasm migrated, static conformance test) — the first two PRs of docs/superpowers/specs/2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html.

**Architecture:** PR1 introduces `crates/riz-wasm`, a guest-side shim crate that owns the stdin line-JSON event loop and exposes `run(handler)` mirroring `lambda_runtime`'s mental model. It wraps the CURRENT envelope and CURRENT `riz_broker` pg ABI — zero `src/` transport changes. Guests become pure handlers; a static conformance test bans wire tokens from example sources.

**Tech Stack:** Rust (host workspace member + wasm32-wasip1 guest builds), serde_json, cargo-nextest, wasmtime (test-side only, already a dep).

## Global Constraints

- Gates before every push: `cargo fmt --all -- --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `./scripts/safety-check.sh --gate` · `cargo nextest run --workspace -E 'not binary(e2e_smoke_all)'` · `cargo nextest run --test e2e_smoke_all` (isolated).
- cargo-nextest only, never `cargo test`.
- `crates/riz-wasm` is OUTSIDE `src/` → exempt from docs/SAFETY.md lint tier by policy (it is guest-side; its `-> !` loop is the sanctioned event loop). Only the riz package opts into `[workspace.lints]` — do not opt riz-wasm in.
- The AWS Lambda + APIGW v2 wire contract at the HTTP boundary must not change. The migrated guests must produce byte-equivalent responses (tests/wasm_examples.rs and the parity suite are the oracle).
- Merge flow per repo convention: branch → push → `gh pr create` → `gh pr merge --squash --admin` (rebase first if "not up to date") → `git pull --ff-only` on main.
- Banned words in all copy/comments: honest/truthful/candid.
- Deviation from spec PR1 text, decided here: the claims-registry entry moves to the PR that lands the R1 web copy (PR12) — `tests/claims_truth.rs` enforces page_text↔test bidirectionally, so an entry with no page yet would fail the reverse check.
- riz-wasm crates.io publish: deferred until PR2 needs it; templates git-dep `{ git = "https://github.com/24X7/riz" }` as the spec's sanctioned stopgap.

---

### Task 1: PR0 — drift fixes

**Files:**
- Modify: `server.json:32`
- Modify: `examples/README.md:3-5`
- Modify: `examples/demo.py:10`
- Modify: `CONTRIBUTING.md:236,253,257,260`
- Modify: `src/config.rs:802-805`
- Modify: `CLAUDE.md` (lockstep list)
- Modify: `docs/plans/v1-roadmap.md` (item #2 supersedes note)
- Modify: `docs/superpowers/specs/2026-06-10-wasm-resource-broker-design.md` (phasing supersedes note)

**Interfaces:** none (prose/comments only; `src/config.rs` edit is a doc comment).

- [ ] **Step 1: verify branch.** Work on `design/lambda-shape-purity-wasm-capability-suite` (already carries the spec commit, rebased on cfbda12). `git status` clean.

- [ ] **Step 2: apply the six drift edits** (exact replacements):

`server.json:32`:
```json
"Six runtimes in one binary: Bun, Node.js, Python, Rust, Go, capability-sandboxed WASM (WASI deny-by-default).",
```

`examples/README.md:3-5` (first paragraph):
```markdown
Seventeen example handlers spanning all six runtimes (Bun, Node.js, Python,
Rust, Go, WASM), both protocols (HTTP, WebSocket), and the authorizer surface.
Each example has its own README under `examples/lambdas/<name>/`.
```

`examples/demo.py:10`:
```
  • All SIX runtimes         Bun, Node.js, Python, Rust, Go, WASM — one envelope
```

`CONTRIBUTING.md:236` — replace the `rust.rs` layout line with:
```
    static_binary.rs   # Rust/Go adapter (spawns user's pre-built binary)
    runtime_api.rs     # per-worker AWS Lambda Runtime API those binaries speak
```
`CONTRIBUTING.md:253` — replace the scaffold-count line with:
```
  templates/           # 9 `riz init <template>` scaffolds (5 HTTP + 3 WebSocket + 1 WASM)
```
`CONTRIBUTING.md:257` — replace with:
```
  lambdas/             # 17 example handlers across all six runtimes
```
`CONTRIBUTING.md:260` — delete the `riz-rust-runtime/` line (dead crate; Rust handlers use the official `lambda_runtime` crate, no riz library).

`src/config.rs:802-805` — replace the Go variant's doc comment with:
```rust
    /// A pre-compiled Go binary using the official `aws-lambda-go` SDK against
    /// riz's per-worker AWS Lambda Runtime API (`src/process/runtime_api.rs`).
    /// Like Rust, the handler IS the executable — there is no module/export
    /// split. Runs via the same `static_binary` spawner as Rust.
```

- [ ] **Step 3: CLAUDE.md lockstep list.** In the "Layout notes" runtime-lockstep sentence, extend the enumerated surfaces to add: `server.json`, `examples/README.md`, `examples/demo.py`, and `templates/`.

- [ ] **Step 4: supersedes notes.**
`docs/plans/v1-roadmap.md` item #2 — append to the item:
```markdown
> **Superseded (2026-07-19):** the stdin-adapter mechanics described here are
> replaced by the riz-wasm shim + RWP wire in
> `docs/superpowers/specs/2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html`;
> the wasm template lands via that spec's PR2.
```
`docs/superpowers/specs/2026-06-10-wasm-resource-broker-design.md` — under the Phasing heading, insert:
```markdown
> **Phasing superseded (2026-07-19):** the threat model, dispatcher ordering and
> closed error set below carry forward unchanged, but the v1.1 KV / v2 S3+Dynamo /
> v2 http_fetch / v3 WIT phasing is replaced by the capability suite + PR sequence
> in `2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html`.
> KV and S3 verbs are no longer planned for this cycle.
```

- [ ] **Step 5: verify no stale counts remain.** Run: `rg -n "Five runtimes|FIVE runtimes|four runtimes|Thirteen|riz-rust-runtime" --iglob '!target' --iglob '!docs/assessments' .` Expected: no hits outside docs/assessments (historical) and the spec's own "current state" narrative.

- [ ] **Step 6: gates + commit.** Run all five gate commands (Global Constraints). Expected: all pass untouched (prose-only changes; config.rs edit is a comment). Commit:
```bash
git add -A && git commit -m "docs(drift): six-runtime truth on every surface; supersedes notes; lockstep list

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 2: PR0 — ship it

- [ ] **Step 1:** `git push -u origin design/lambda-shape-purity-wasm-capability-suite`
- [ ] **Step 2:** `gh pr create` — title `docs(spec+drift): shape-purity/capability-suite spec (HTML) + six-runtime drift fixes (PR0)`; body summarizes: spec as source of record, six drift fixes, supersedes notes, lockstep additions; ends with the 🤖 footer.
- [ ] **Step 3:** `gh pr merge --squash --admin`; then `git checkout main && git pull --ff-only`.

### Task 3: PR1 — riz-wasm crate: envelope + context (TDD)

**Files:**
- Create: `crates/riz-wasm/Cargo.toml`
- Create: `crates/riz-wasm/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` += `"crates/riz-wasm"`)

**Interfaces:**
- Produces (consumed by Tasks 4-6 and every future wasm guest):
  - `riz_wasm::run(handler: fn(Event, Context) -> Result<Response, Error>) -> !`
  - `Event::raw(&self) -> &serde_json::Value` (full APIGW v2 event)
  - `Context::{function_name(&self) -> &str, request_id(&self) -> &str, deadline_ms(&self) -> i64, remaining_time(&self) -> std::time::Duration}`
  - `Response::from(serde_json::Value)` (the full Lambda proxy response object)
  - `Error = Box<dyn std::error::Error>`
  - exported symbol `riz_abi_v1` in built guests

- [ ] **Step 1: branch.** `git checkout -b feat/riz-wasm-shim` off updated main.

- [ ] **Step 2: crate scaffold.**

`crates/riz-wasm/Cargo.toml`:
```toml
[package]
name = "riz-wasm"
version = "0.1.0"
edition = "2021"
description = "Author AWS-Lambda-shaped handlers for riz's WASM runtime — the shim owns the event loop and the capability ABI"
license = "MIT"
repository = "https://github.com/24X7/riz"

[dependencies]
serde_json = "1"
```
Workspace `Cargo.toml`: add `"crates/riz-wasm"` to `members`.

- [ ] **Step 3: failing unit tests first** (in `lib.rs` `#[cfg(test)]`): envelope with wrapper populates Context; bare event falls back (empty function name → "unknown"? No — spec: Context fields; function_name falls back to `"unknown"`, request_id to `""`); malformed line → the 400 error response line; handler Err → 500 response line; `remaining_time` clamps at zero. Write tests against an internal pure fn `process_line(line: &str, now_ms: i64, handler) -> String` so the loop stays untested-thin.

- [ ] **Step 4: run tests, verify FAIL** (`cargo nextest run -p riz-wasm`), **then implement**:

```rust
//! riz-wasm — author an AWS-Lambda-shaped handler; the shim owns the wire.
//!
//! A guest's whole main.rs is:
//! ```ignore
//! fn handler(event: riz_wasm::Event, ctx: riz_wasm::Context)
//!     -> Result<riz_wasm::Response, riz_wasm::Error> { ... }
//! fn main() { riz_wasm::run(handler) }
//! ```
//! Wire v1 (selected when RIZ_WIRE is unset or "1"): one JSON line per
//! invocation on stdin — `{ event, __riz_deadline_ms, __riz_function_name }`
//! with a bare-event fallback — one Lambda proxy-response JSON line on stdout.
//! An unknown RIZ_WIRE value fails closed at startup (exit 78) so wire skew
//! can never desync the pipe.

use std::io::{BufRead, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Error = Box<dyn std::error::Error>;

pub struct Event(serde_json::Value);
impl Event {
    pub fn raw(&self) -> &serde_json::Value { &self.0 }
    pub fn into_inner(self) -> serde_json::Value { self.0 }
}

pub struct Response(serde_json::Value);
impl From<serde_json::Value> for Response {
    fn from(v: serde_json::Value) -> Self { Response(v) }
}

pub struct Context { function_name: String, request_id: String, deadline_ms: i64 }
impl Context {
    pub fn function_name(&self) -> &str { &self.function_name }
    /// From event.requestContext.requestId; empty string when the event lacks it.
    pub fn request_id(&self) -> &str { &self.request_id }
    pub fn deadline_ms(&self) -> i64 { self.deadline_ms }
    pub fn remaining_time(&self) -> Duration { /* deadline - now, clamped ≥ 0 */ }
}

pub type Handler = fn(Event, Context) -> Result<Response, Error>;

/// Exported ABI marker — the host's load-time handshake (checked from PR10 on).
#[no_mangle]
pub extern "C" fn riz_abi_v1() {}

pub fn run(handler: Handler) -> ! { /* RIZ_WIRE check → line loop → process_line */ }

fn process_line(line: &str, now_ms: i64, handler: Handler) -> String { /* envelope parse, ctx build, handler call, 400/500 fallbacks */ }
```
(Complete bodies: envelope parse mirrors echo-wasm's current logic verbatim — `parsed.get("event").unwrap_or(&parsed)`, function_name default `"unknown"`, deadline default 0; malformed JSON → the canonical 400 line `{"statusCode":400,"headers":{"content-type":"application/json"},"multiValueHeaders":{},"body":"{\"message\":\"bad event json\"}","isBase64Encoded":false,"cookies":[]}`; handler `Err(e)` → same shape with 500 and `{"message":"handler error: <e>"}`. `run` reads RIZ_WIRE: unset/`"1"` → loop; anything else → `eprintln!` + `exit(78)`. Loop = current guests' loop verbatim, calling `process_line` with `SystemTime::now()` millis, and `std::process::exit(0)` on stdin EOF so the signature is honestly `-> !`.)

- [ ] **Step 5: tests pass** (`cargo nextest run -p riz-wasm`), then full workspace clippy/fmt. Commit: `feat(riz-wasm): shim crate — run(handler), envelope v1, RIZ_WIRE fail-closed, abi marker`.

### Task 4: PR1 — cap::pg over the CURRENT ABI

**Files:** Modify: `crates/riz-wasm/src/lib.rs` (add `pub mod cap`)

**Interfaces:**
- Produces: `riz_wasm::cap::pg::query(grant: &str, sql: &str, params: &[serde_json::Value]) -> Result<Vec<serde_json::Value>, CapError>`; `CapError { code: String, message: String }` mirroring the closed error set; pure `cap::pg::decode_response(&[u8]) -> Result<Vec<serde_json::Value>, CapError>`.

- [ ] **Step 1: failing tests** for `decode_response`: `{"ok":true,"rows":[...],"row_count":N}` → rows; `{"ok":false,"error":{"code":"denied","message":"m"}}` → CapError{denied}; garbage → CapError{code:"bad_request"}-shaped local error.
- [ ] **Step 2: implement.** `decode_response` pure; `query` is `#[cfg(target_arch = "wasm32")]` calling the CURRENT two-call ABI (`riz_broker.pg_query` then `read_response` with retry-on-short-buffer, exactly the protocol in tests/fixtures/broker-wasm/src/main.rs:15-46), then `decode_response`; on non-wasm targets `query` returns `CapError{code:"denied", message:"capabilities are only available inside the riz wasm host"}` so the crate builds/tests on the host. End-to-end exercise arrives with PR4's fixtures; unit tests pin the envelope today.
- [ ] **Step 3: tests pass; commit** `feat(riz-wasm): cap::pg over the current riz_broker ABI`.

### Task 5: PR1 — migrate echo-wasm and orders-wasm

**Files:**
- Modify: `examples/lambdas/echo-wasm/Cargo.toml` (+ `riz-wasm = { path = "../../../crates/riz-wasm" }`)
- Modify: `examples/lambdas/echo-wasm/src/main.rs` (handler-only rewrite)
- Modify: `examples/lambdas/orders-wasm/Cargo.toml`, `src/main.rs` (same)

**Interfaces:**
- Consumes: `riz_wasm::{run, Event, Context, Response, Error}` from Task 3.

- [ ] **Step 1: rewrite echo-wasm** — doc header flips from wire description to the handler contract; `main` becomes `fn main() { riz_wasm::run(handler) }`; `handler(event, ctx)` keeps the EXACT response construction of today's `handle()` (status honoring `?status=`, arn, `req-{deadline}` request-id fallback via `ctx.deadline_ms()`, every echoed field, the `x-riz-echo` header, the `sid=abc` cookie). ~30 lines of protocol deleted.
- [ ] **Step 2: build + behavioral verify.** `cargo build --release --target wasm32-wasip1 --manifest-path examples/lambdas/echo-wasm/Cargo.toml` then `cargo nextest run --test wasm_examples --test runtime_parity_echo` (parity leg auto-skips if unbuilt — it must NOT skip here). Expected: PASS unchanged.
- [ ] **Step 3: rewrite orders-wasm** the same way — `price_order` and `response()` byte-identical; handler parses `event.raw().get("body")`. Build; `cargo nextest run --test wasm_examples`. Expected: the 200-pricing and 422-validation assertions pass unchanged.
- [ ] **Step 4: marker export check.** Add to `tests/wasm_examples.rs` a test that loads the built echo-wasm.wasm with the workspace's `wasmtime` and asserts a `riz_abi_v1` export exists (same skip-if-unbuilt guard as its siblings). If the export was GC'd by wasm-ld, fix by adding `.cargo/config.toml` in each guest: `[target.wasm32-wasip1] rustflags = ["-C", "link-args=--export=riz_abi_v1"]` — re-run until PASS.
- [ ] **Step 5: commit** `feat(examples): echo-wasm + orders-wasm author as pure Lambda handlers on riz-wasm`.

### Task 6: PR1 — static conformance test (examples scope)

**Files:** Create: `tests/lambda_shape_conformance.rs`

**Interfaces:** standalone test; PR2 extends its scope to `templates/`.

- [ ] **Step 1: write the test (failing-first check).** Walk `examples/lambdas/*/` + `examples/ai-chat/` + `examples/typescript-todo/` source files (`.ts .mjs .js .py .rs .go`), skipping `target/`, `node_modules/`, `__pycache__/`. Banned tokens by extension — `.ts/.mjs/.js`: `process.stdin`, `readline(`; `.py`: `sys.stdin`; `.rs`: `io::stdin`, `wasm_import_module`; all: `__riz_deadline_ms`, `__riz_function_name`. Allowlist: a `const ALLOW: &[(&str, &str, &str)]` of (path-suffix, token, justification) entries — starts EMPTY; any future entry requires a justification string (mirrors the SAFETY.md allow-comment discipline). On hit: fail listing file:line, token, and the fix ("author a handler; the adapter/shim owns the wire").
- [ ] **Step 2: run.** `cargo nextest run --test lambda_shape_conformance`. Expected after Task 5: PASS (the two wasm guests were the only violators). If ai-chat/typescript-todo surface a hit, migrate the code if it's wire-shaped, or add an ALLOW entry with justification if legitimate (e.g. a CLI helper script) — prefer migration.
- [ ] **Step 3: commit** `test(conformance): examples must author Lambda handlers — wire tokens banned`.

### Task 7: PR1 — gates + ship

- [ ] **Step 1: full gates** (all five commands). e2e_smoke_all builds the wasm fleet itself — expect PASS with the migrated guests.
- [ ] **Step 2:** push `feat/riz-wasm-shim`; `gh pr create` (title `feat(riz-wasm): handler-only WASM authoring — shim crate, guest migration, conformance test (PR1)`); merge `--squash --admin`; `git checkout main && git pull --ff-only`.

## Self-Review (done at write time)

1. **Spec coverage:** PR0 items all mapped (gitignore/binary item verified already-fixed upstream — dropped with note); PR1 = shim (Task 3), current-ABI cap wrap (Task 4), guest migration (Task 5), static conformance + allowlist mechanism (Task 6), marker export (Task 5.4), RIZ_WIRE fail-closed (Task 3), crates.io deferral + claims-entry deferral recorded in Global Constraints.
2. **Placeholder scan:** the two `/* ... */` bodies in Task 3's listing are specified exhaustively in the adjacent prose (exact fallback lines, exit code, EOF behavior) — the implementer has the full contract; no TBDs.
3. **Type consistency:** `run/Event/Context/Response/Error/cap::pg::query` names match across Tasks 3-6.
