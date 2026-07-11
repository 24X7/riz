# Safety-Critical Rules — NASA's Power of 10, Adapted for Rust

riz is an always-on host daemon that supervises untrusted workloads: it accepts
network input, spawns and sandboxes worker processes, and must degrade
gracefully rather than fall over. That is the risk profile NASA/JPL's
[Power of 10](https://en.wikipedia.org/wiki/The_Power_of_10:_Rules_for_Developing_Safety-Critical_Code)
(Gerard Holzmann, 2006) was written for — code whose failure mode matters more
than its feature velocity. This document adapts those ten rules to Rust and
binds them to this codebase.

Two framing notes:

1. **Rust mechanizes several of the rules.** Memory safety (rule 9), checked
   return values (rule 7), and scope minimality (rule 6) are partly or wholly
   the compiler's job in Rust. Where that is true, the adaptation says so and
   enforces the *residue* the compiler does not cover.
2. **Some rules were written for flight software and do not map literally**
   to a network daemon (rule 3's "no heap allocation after init"). Where the
   letter does not transfer, we enforce the *intent* and state the divergence
   explicitly rather than pretending compliance.

## Scope

The rules bind **code compiled into the `riz` binary**: the root package's
`src/` tree (`cargo clippy --lib --bins`). They do **not** bind:

- `examples/lambdas/*` — user-facing samples; they demonstrate the official
  AWS runtime APIs, and safety scaffolding there would obscure the demo.
- `tests/`, `benches/`, `#[cfg(test)]` modules, `tests/fixtures/*` — test
  code *asserts* by panicking; that is its failure signal, not a defect.
- `templates/`, `web/`, scripts — not compiled into the binary.

## Enforcement tiers

Every rule is enforced at the strongest tier the current code supports. A rule
is never "documented only" if a lint can carry it.

| Tier | Mechanism | When it runs |
|---|---|---|
| **enforced** | `[workspace.lints]` in `Cargo.toml`, level `deny` | every compile, IDE, CI (`-D warnings`) |
| **gate** | `scripts/safety-check.sh --gate` (`--lib --bins`, `-D`) | CI, every push/PR |
| **ratchet** | `scripts/safety-check.sh` report — counts may only go down | safety review loop |
| **review** | not mechanizable — criteria for code review + the recurring Power-of-10 assessment | every PR touching `src/` |

**Status (2026-07-06): the ratchet tier is empty.** The 253 sites baselined on
2026-07-03 are at zero (see "Ratchet baseline" below). Every lint that carried
a rule now sits in the `enforced` or `gate` tier; the `review` tier remains
permanently active for what lints cannot see.

**Promotion protocol:** when a ratchet lint's count reaches zero, it moves to
the CI gate; once test code also passes it on `--all-targets` (or a
`clippy.toml` `allow-*-in-tests` option fully covers it), it moves to
`[workspace.lints] deny`. Most fail-loud lints stay in the `gate` tier: their
`allow-*-in-tests` option covers `#[cfg(test)]` and `#[test]` functions but not
integration-test *helper* functions, so `--all-targets` still flags test code.
Counts must never go up: if a PR adds a violation site, it reverts or carries
a justified `#[allow]` (see Deviations).

---

## The ten rules

### Rule 1 — Simple control flow

> *NASA: no `goto`, no `setjmp`/`longjmp`, no recursion.*

Rust has no goto and no longjmp, but it has two moral equivalents:
**panic-as-control-flow** and **process exit from arbitrary depth**.

- No `panic!` in production code — a panic is a detected bug, never a branch.
  `catch_unwind` only at task/request boundaries (supervisors), never to
  implement logic. — **gate** (`clippy::panic`)
- `std::process::exit` only at the CLI top level (`main.rs` argument/startup
  errors), never from library code. The three `doctor` verdict sites carry
  statement-scoped `#[allow(clippy::exit)]`. — **enforced** (`clippy::exit`)
- No recursion in request-handling paths. Recursion over *external* input
  (nested JSON, zip entries, directory trees) must either use the parser's
  depth limit or carry an explicit documented depth cap. — **review**
  (rustc's `unconditional_recursion` catches only the trivial case)

### Rule 2 — All loops have a fixed upper bound

> *NASA: a static analyzer must be able to prove every loop terminates.*

A daemon legitimately contains loops meant to run forever. The adaptation
splits loops into two kinds and forbids everything in between:

- **Bounded loops** — iterator chains over finite collections (structurally
  bounded, the preferred form), or `loop`/`while` with a provable exit.
  Retries must have an attempt cap and a backoff ceiling.
- **Supervised event loops** — the accept loop, worker supervisors, watch
  loops, the TUI event loop. Each must: (a) sit at the top level of a spawned
  task, (b) yield (`await`) every iteration, (c) do bounded work per
  iteration, (d) exit on the shutdown signal.

Anything else — a spin that can neither finish nor be shut down — is a defect.
— **enforced** (`clippy::infinite_loop`, `clippy::maybe_infinite_iter`);
the event-loop contract for the ~12 bare `loop {` sites is **review**.

### Rule 3 — Predictable memory: no unbounded growth from input

> *NASA: no heap allocation after initialization.*

**Stated divergence:** a network daemon cannot serve traffic without
allocating. The rule's intent is that memory behavior is predictable and a
single input (or a slow consumer) cannot exhaust the process. Adaptation:

- No unbounded queues: `tokio::sync::mpsc::channel` (bounded) over
  `unbounded_channel`; backpressure is handled explicitly, not deferred to
  the allocator. — **enforced** (`clippy::disallowed_methods`, list in
  `clippy.toml`)
- Every buffer that grows from remote input carries an explicit cap (body
  size limits, header caps, zip-extraction limits). — **review**
- Caches are bounded (`moka` with capacity + TTL). — **review**
- Resources are released by `Drop`, never leaked. — **enforced**
  (`clippy::mem_forget`)
- Backstop: per-worker `setrlimit` caps (`process/safety.rs`) bound the
  blast radius when a *workload* misbehaves.

### Rule 4 — Short functions

> *NASA: no function longer than one printed page (~60 lines).*

- Functions ≤ 100 lines (`too-many-lines-threshold` in `clippy.toml`).
  All of `src/` is under the ceiling; a future tightening toward NASA's ~60
  would re-open a ratchet. — **gate** (`clippy::too_many_lines`)
- Cognitive complexity ≤ 25 per function (`cognitive-complexity-threshold`).
  — **gate** (`clippy::cognitive_complexity`)

### Rule 5 — Assertion density

> *NASA: minimum two runtime assertions per function; assertions are
> side-effect free and trigger recovery, not aborts.*

Rust's primary assertion engine is the **type system** — an invariant encoded
in a type (newtype, non-empty wrapper, enum instead of boolean soup) is an
assertion checked at compile time on every call, which beats two runtime
checks. The residue:

- Boundary inputs (config, HTTP, IPC, env) are validated where they enter,
  returning `Err` — recovery, not abort — or are parsed into types that make
  the invalid state unrepresentable.
- Internal invariants use `debug_assert!` (free in release) or return
  errors; assertions never have side effects.
- Integer overflow is checked in release builds (`overflow-checks = true` in
  `Cargo.toml`): every arithmetic operation is a runtime assertion. A panic
  is diagnosable; silent wraparound is corruption. — **enforced** (profile)
- Arithmetic on values derived from external input uses
  `checked_*`/`saturating_*` forms so the *recovery* is explicit rather than
  a panic. — **gate** (`clippy::arithmetic_side_effects`)

### Rule 6 — Smallest possible scope for data

> *NASA: declare data objects at the smallest level of scope.*

Rust defaults to private and immutable, which is most of this rule. Residue:

- No mutable statics (`static_mut_refs` is deny-by-default territory; global
  state lives in `AppState` behind explicit sync types).
- `pub` only where the lib/test boundary requires it; items private to their
  module by default. Dead code is deleted, not `#[allow]`ed. — **review** +
  rustc's default `unused_*`/`dead_code` lints under CI `-D warnings`

### Rule 7 — Check every return value

> *NASA: check the return value of every non-void function, or explicitly
> cast to void; validate parameters inside each function.*

`Result` + `#[must_use]` mechanize the C-era rule; `-D warnings` in CI makes
`unused_must_use` a hard error. The residue is discipline about *how* results
are checked:

- `let _ = …` is the sanctioned "cast to void" — an explicit, greppable
  discard. It should carry a short comment saying why discarding is correct
  (e.g. send-on-closed-channel during shutdown).
- No `.unwrap()` in production paths — an unwrap is an unchecked return
  value wearing a disguise. — **gate** (`clippy::unwrap_used`)
- `.expect()` only for init-phase impossibilities (before the server accepts
  traffic), with a message stating the invariant; request-path code returns
  `Err`. — **gate** (`clippy::expect_used`)
- `unreachable!` requires a proof in the message or a refactor to an error
  return. — **gate** (`clippy::unreachable`)
- A dropped `Future` is not a discarded result — it is work that silently
  never ran. — **enforced** (`clippy::let_underscore_future`)

### Rule 8 — Restricted macro / conditional-compilation use

> *NASA: preprocessor limited to includes and simple, side-effect-free
> macros; no token pasting or variadic macros.*

Rust macros are hygienic and type-checked at expansion, so the C-era dangers
mostly do not transfer. What does:

- No placeholder or debug constructs in the binary: `todo!`,
  `unimplemented!`, `dbg!`. — **enforced** (`clippy::todo`,
  `clippy::unimplemented`, `clippy::dbg_macro`)
- Declarative macros only where a function or generic cannot express the
  pattern; this repo authors no proc-macros. Derives (serde, clap,
  thiserror) are fine — they are type-checked codegen, not textual
  substitution. — **review**
- `cfg` gates only at real platform boundaries (`cfg(unix)`,
  `cfg(target_os = "linux")`) — never to fork feature behavior. — **review**

### Rule 9 — Pointer discipline

> *NASA: at most one level of dereferencing; no function pointers.*

The borrow checker is this rule's static analyzer: references cannot dangle,
alias mutably, or leak across threads unsynchronized. The Rust residue is
everything that *escapes* that analysis:

- `unsafe` is denied workspace-wide. Each use is a local, justified
  exception: `#[allow(unsafe_code)]` at the site plus a `// SAFETY:` proof,
  one unsafe operation per block. Current uses: exactly the `pre_exec`
  sandbox hooks in `process/` (setrlimit/prctl/landlock between fork and
  exec — see `pool.rs`). — **enforced** (`unsafe_code`,
  `clippy::undocumented_unsafe_blocks`, `clippy::multiple_unsafe_ops_per_block`,
  `unsafe_op_in_unsafe_fn`)
- No panicking access: `[]` indexing and slicing on runtime data are
  unchecked dereferences in spirit — use `.get()` and handle `None`. —
  **gate** (`clippy::indexing_slicing`)
- **Stated divergence:** function pointers and closures are permitted. The
  C rule exists because function pointers defeat C static analyzers; rustc
  type-checks them completely.

### Rule 10 — All warnings, all analyzers, always

> *NASA: compile with all warnings enabled, warnings as errors; analyze with
> multiple static analyzers daily; rewrite rather than suppress.*

Every push/PR runs, in order, all gating:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings` (includes the
   enforced lint tier)
3. `scripts/safety-check.sh --gate` (production-only zero-tolerance tier)
4. `cargo nextest run` — full suite, process-per-test isolation with leak
   detection (a child outliving its test fails the run), plus the isolated
   e2e fleet smoke
5. The recurring Power-of-10 review loop — a second, semantic analyzer over
   what lints cannot see (bounds on buffers, event-loop contracts, assertion
   placement); findings land in `docs/assessments/`.

Suppressing a diagnostic (`#[allow]`) is a deviation, not a fix — see below.

---

## Deviations

Any `#[allow]` of a safety lint in `src/` must:

1. sit at the smallest scope (statement/function, never module/crate),
2. carry a comment on adjacent lines stating *why the rule does not apply
   here* (for `unsafe`: a `// SAFETY:` proof),
3. survive review as an exception, not a convenience.

`grep -rn "#\[allow(" src/` is the deviation register; the review loop audits
it. The safety-lint entries as of 2026-07-06:

| Lint | Sites | Where | Justification |
|---|---|---|---|
| `unsafe_code` | 5 | `process/safety.rs` (4), `process/pool.rs` (1) | `pre_exec` sandbox hooks (setrlimit/prctl/landlock between fork and exec); each carries a `// SAFETY:` proof |
| `clippy::exit` | 3 | `main.rs` doctor verdicts | top-level CLI contract: summary on stdout + exit code; an `anyhow::Err` would duplicate the verdict on stderr |

(`clippy::too_many_arguments` in `system/openai_compat.rs` is outside this
policy — it is not a Power-of-10 lint.)

## Ratchet baseline — closed 2026-07-06

The burn-down is complete: **253 unique production-code sites → 0**. The
baseline was first measured at 239 on 2026-07-03; new A2A code (PRs #24/#26)
landing on top raised it to 253 before the sweep cleared it.

| Lint | Rule | Baseline sites | Final tier |
|---|---|---|---|
| `clippy::indexing_slicing` | 9 | 98 | gate |
| `clippy::arithmetic_side_effects` | 5 | 67 | gate |
| `clippy::expect_used` | 7 | 20 | gate |
| `clippy::unwrap_used` | 7 | 15 | gate |
| `clippy::too_many_lines` | 4 | 15 | gate |
| `clippy::cognitive_complexity` | 4 | 13 | gate |
| `clippy::unreachable` | 7 | 6 | gate |
| `clippy::exit` | 1 | 3 | enforced |
| `clippy::disallowed_methods` (unbounded channels) | 3 | 2 | enforced |

Cleared risk-profile-first over eight PRs — request path (#30 system, #31
static_files, #32 cors/router/auth/ws), sandbox (#33 process/broker/runtime),
control plane (#37 llm, #41 config/deploy/etc.), then dev-only tui/main panic
paths (#42) and the structure pass (this PR). Per-iteration counts, findings,
and review-tier work are in `docs/assessments/2026-07-03-power-of-10.md`. The
ratchet tier is now empty; `scripts/safety-check.sh` (no args) confirms it.
