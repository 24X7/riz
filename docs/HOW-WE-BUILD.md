# How We Build riz — an agent-native engineering method

> riz is a self-hosted AWS Lambda runtime in one ~10 MB Rust binary, built largely by an AI agent
> working autonomously. This document is both the story and the playbook — the method we actually
> use, not an aspirational description of how we wish we worked.

The short version: a human sets direction; an AI agent does the engineering. The agent reads memory,
operates the repo directly, runs tests, commits, and ships — in a self-paced loop that idles only
when there is genuinely nothing worth doing next. That is not a future state. That is how every
commit in this repo was made.

---

## 1. Persistent memory / context

Every session, the agent reads a file-based memory index before touching any code:

```
.claude/projects/.../memory/MEMORY.md     ← index of named decisions
.claude/projects/.../memory/feedback_*.md ← durable preferences
.claude/projects/.../memory/project_*.md  ← scope + constraint records
```

The index entries read as hard constraints — not suggestions:

- `cargo-nextest only, never cargo test` — the agent will not use `cargo test` anywhere in the repo.
- `TUI driven only by --dev` — `riz run` is headless; no `--no-tui`, no TTY detection.
- `Rust over Go for always-on services` — host daemon is Rust + Ratatui + Clap, always.
- `Scope: AWS HTTP/WS Lambdas only` — SQS/SNS/S3/EventBridge are out of scope, not gaps.
- `Greenfield: no backwards compat` — v0.1 has zero users; rename and delete cleanly.

When the human makes a correction or decision mid-session, it gets written down immediately and
becomes a constraint for all future sessions. Knowledge does not live in conversation history that
compacts away. It lives in files.

This is how you get consistent behavior across dozens of sessions without re-litigating the same
decisions. The agent doesn't remember — it reads.

---

## 2. Tools — real engineering, not suggestions

The agent operates the repo the same way a developer does. Not by describing what to do; by doing
it:

- **File read/edit** — reads source, edits in place, respects file boundaries. Every change is exact.
- **Shell** — runs `cargo nextest run`, `cargo build --release`, `git log`, `curl` against a live
  riz instance, `riz mcp inspect`. Real output, real errors.
- **Git** — stages specific files, writes commit messages that follow the repo's established
  prefix-style (`feat(llm):`, `fix(ws):`, `docs:`, `test:`), creates commits.
- **Web** — reads the live `riz.dev` site when a claim needs verification; checks external docs
  when integrating a new spec.
- **Tests** — `cargo nextest run` only. 778 tests, ~60 seconds. The suite is the ground truth.
  If it's green, it shipped.

The effect is that a feature goes from idea to committed, tested, integrated code without a human
touching an editor. The human reviews; the agent builds.

---

## 3. `/loop` — autonomous build sessions

The highest-leverage capability in the method. A `/loop` session works like this:

1. The agent reads memory and evaluates the current state of the repo.
2. It identifies the highest-value next item — based on the roadmap, open plan tasks, or the most
   glaring gap between what the site claims and what the tests prove.
3. It builds the item end-to-end: implementation, tests (red → green), integration, commit.
4. It repeats. No prompt between iterations.

This is how features actually land. Five runtimes, the MCP server, the LLM gateway, the WASM
sandbox, the TUI, 778 tests across 47 integration files — all of it fell out of loop sessions.
The commit log reflects this cadence: `feat(llm): LLM gateway core`, `feat(wasm): capability-
sandboxed WASM runtime`, `feat(llm): SSE streaming`, `feat(llm): budget caps + cost telemetry` —
consecutive, complete, tested.

The human's role in a loop session is to start it, stay available for questions, and review commits
as they land. The agent self-paces; it stops when the roadmap item is done or when it hits a
decision that needs human input.

Loop sessions are particularly effective for:
- Working through a phased plan where tasks have clear completion criteria.
- Closing a gap between a site claim and a passing test.
- Chasing down a failing test and fixing its root cause with full context.

---

## 4. `/btw` — side-affirmations

Not a formal command — a pattern. Mid-loop, the human drops a correction or preference in plain
language without interrupting the work:

> `/btw` cargo nextest only, never cargo test
> `/btw` keep `riz run` headless by default — TUI is --dev only
> `/btw` don't touch the WebSocket handler interface, that's the AWS wire contract

The agent folds it in immediately, writes it to memory if it's durable, and continues. The loop
doesn't derail. The decision is locked.

This is how the project's constraint ledger grew: not through planning sessions, but through the
human noticing a drift and anchoring it in real time. `/btw` is the lightest possible steering
mechanism — one line, zero ceremony, permanent effect.

---

## 5. Planning + task lists

Big work doesn't start with code. It starts with a written plan.

Plans live in `docs/superpowers/plans/` and follow a consistent shape: goal, current-state
assessment with a gap table, execution order with rationale, per-phase work decomposed into
concrete files and tasks, open decisions explicitly listed. The 2026-06-09 claims-truth plan spans
six phases — homepage repositioning, test-trust foundation, observability, auth, AI-native
examples, WASM hardening — with architecture decisions resolved inline and a stop gate at the
bottom: "this plan is now on paper. Stop here and compact before proceeding."

The stop gate matters. Planning and execution are separate passes. The plan gets written, reviewed,
and locked before a single file is touched. Then the agent expands each phase into bite-sized TDD
tasks and executes them one by one with review between.

The website is treated as the spec. Literally: "a claim on the page is a contract backed by a
test." Every capability listed on `riz.dev` maps to a named product/integration test in
`tests/claims/registry.toml`. A false claim turns a test red. A red test means either build the
feature or change the copy. No claim ships untested. The claims registry enforces this both ways —
a live (non-ribboned) claim must have a passing test; a coming-soon capability must be visibly
greyed with a ribbon and linked to a roadmap item.

Task tracking is plain markdown checklists in the plan files — no external project management.
The plan is the backlog; checkboxes are the burn-down.

---

## 6. Superpowers — parallel agents, TDD, structured planning

The skills the agent reaches for when work gets complex. We want to use these more aggressively.

**`superpowers:dispatching-parallel-agents` ("party mode")** — when a phase has genuinely
independent subproblems (e.g., Phase 2's observability work splits across host emitter, IPC
channel, OTLP encoding, Datadog exporter, and TUI panel), dispatch multiple focused subagents
concurrently. Each agent owns one domain, works in isolation, and produces a commit. The
orchestrator merges and verifies. Work that would take four sequential sessions takes one parallel
one. We under-use this today; the next multi-domain phase should start with explicit agent
decomposition before the first line of code.

**`superpowers:test-driven-development`** — strict red → green → commit. Write the failing test
first, against the behavior you intend. Watch it fail. Implement until it passes. Commit both
together. This is not optional style guidance — it is enforced by the method. The claims registry
only accepts `proven` status when a named passing test exists. Any feature whose test was written
after the implementation is suspect; any implementation without a test is, by definition, an
unproven claim.

**`superpowers:writing-plans`** — before executing any multi-step work, produce a written plan
with the structure described in §5: goal, gap assessment, execution order, per-phase decomposition,
open decisions. Plans are first-class artifacts, committed to `docs/superpowers/plans/` with a
date-stamped filename. The rule: the plan is on paper before the first `Edit` call.

**`superpowers:verification-before-completion`** — before claiming a feature is done, before
creating a PR, the agent runs the full suite (`cargo nextest run`) and confirms green output.
Evidence before assertions. A session that ends with "should be working" instead of "778 tests
passed" is not done.

**`superpowers:systematic-debugging`** — when a test fails or behavior is unexpected, diagnose
before patching. Read the actual error. Trace the call path. Understand the root cause. Apply the
minimal fix. This prevents the class of bugs where a quick patch makes one test green and breaks
two others.

The pattern the team wants to cement: every significant feature starts with `superpowers:writing-
plans`, executes via `superpowers:subagent-driven-development` with parallel dispatch where
possible, and closes with `superpowers:verification-before-completion`. The loop skill handles
autonomous pacing between those checkpoints.

---

## Why this matters

Riz exists to make your existing HTTP/WS Lambda handlers agent-callable — no rewrite, no cloud
bill. The MCP server at `/_riz/mcp` turns every function in `riz.toml` into a tool an LLM can
invoke. The same substrate that lets agents call your APIs is the infrastructure that makes
agent-driven engineering possible.

We are not using agent tooling to add a feature to a manually-maintained codebase. The method
described here — persistent memory, direct tool use, autonomous loops, parallel subagents, claims-
backed tests — is the native mode of agent-driven software development. riz is built by agents
using MCP-style tool access to a repo. riz is operated by agents using MCP-style tool access to
your APIs. The product and the method are the same thing.

That is the thesis. The agent era is not a layer you add on top of existing software. It is a
substrate you build into the foundation. riz is ours.

---

*Last updated: 2026-06-09*
