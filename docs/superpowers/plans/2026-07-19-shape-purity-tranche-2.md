# Shape-purity Tranche 2 (PR2 + PR3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** PR2 — the six-template scaffold set behind a renamed `riz new`, enforced by conformance + a scaffold-boot smoke binary. PR3 — examples become showcase-only (parity legs demoted to fixtures, ping/accounts deleted).

**Architecture:** Pure rename/rewrite over the existing `template_fetch` machinery (`RIZ_TEMPLATE_REPO` already supports hermetic local scaffolding). No transport changes. The wasm-rust template is riz-wasm's first consumer via a git dep; the smoke test patches it to the local crate so it verifies current code.

**Tech Stack:** Rust (clap subcommand rename), TypeScript on Bun + Node ≥ 22.18 native type stripping, cargo-nextest isolated binaries.

## Global Constraints

- Same five gates as Tranche 1; merge via `gh pr merge --squash --admin`.
- Verified by sweep: NO claims-registry `page_text` or drift-guard pins any `riz init`/template-name copy — web edits are safe. `site_html()` reads only `web/*.html`.
- CI + CLAUDE.md nextest filters move in lockstep: `-E 'not binary(e2e_smoke_all) and not binary(template_smoke_all)'` + a dedicated `cargo nextest run --test template_smoke_all` step (`.github/workflows/ci.yml:101,108`; `CLAUDE.md:40-41`; CONTRIBUTING isolated-binary note).
- CHANGELOG gets a new entry; released entries are history, never rewritten.
- Node floor for typescript-node: **≥ 22.18** (README-stated + scaffold-time preflight warning). Handler uses the explicit-path form `handler = "./index.ts"` because node's auto-extension is `.mjs` (config.rs `module_and_export`).

---

### Task 1: PR2 — template directories (renames, rewrites, deletions)

**Files:**
- `git mv`: typescript-http→typescript-bun, nodejs-http→typescript-node, python-http→python, rust-http→rust, go-http→go, wasm-http→wasm-rust (all under `templates/`)
- `git rm -r`: templates/{typescript,python,rust}-websocket
- Rewrite: `templates/typescript-node/index.ts` (from index.mjs — real TS, typed via `@types/aws-lambda`), `+package.json`; `templates/typescript-bun/index.ts` (add types), `+package.json`; `templates/wasm-rust/src/main.rs` (on riz-wasm), `templates/wasm-rust/Cargo.toml` (`riz-wasm = { git = "https://github.com/24X7/riz" }`), riz.toml (+ commented capability grant block); `templates/go/riz.toml` (+ `[server]` block); every README (new H1, floor note for node, "WebSocket variant → examples/chat" pointer)

- [ ] Steps: perform moves/deletes; apply rewrites; every README H1 matches its new dir name; typescript-node riz.toml sets `runtime = "node"`, `handler = "./index.ts"`; wasm-rust handler path `./target/wasm32-wasip1/release/hello.wasm` (package name `hello`); commit.

### Task 2: PR2 — src: BUILTINS + `riz new`

**Files:** `src/template_fetch.rs` (BUILTINS rows + module docs + unknown-spec error + unit tests `:448-573`), `src/main.rs` (`Init`→`New` variant + dispatch `:1205`, help `:41-42,107-118`, `print_template_list` two sections, `run_init`→`run_new`, commit msg `:357` → "riz new", doctor hint `:758`, mcp hint `:1170`), `src/config.rs:855-871` error hints.

**Interfaces:** BUILTINS stays `(name, subdir, scenario, language)`; templates = rows whose subdir starts with `templates/`, example starters = rows under `examples/` — `print_template_list` splits on that. Labels: "typescript-bun · HTTP · TypeScript on Bun", "typescript-node · HTTP · TypeScript on Node.js (type stripping, ≥ 22.18)", "wasm-rust · HTTP · Rust → wasm32-wasip1 on riz-wasm", plain "python/rust/go".

- [ ] Steps: apply renames; `run_new` gains a typescript-node preflight (`node --version` parsed; warn to stderr when absent or < 22.18 — never a hard failure); `print_next_steps` prints `cargo build --release --target wasm32-wasip1` when the scaffold's riz.toml contains `runtime = "wasm"`; clippy+fmt; commit.

### Task 3: PR2 — test updates + conformance extension

**Files:** `tests/cli_init.rs` (marker-file table `:55-61` → six rows incl. typescript-node `index.ts`; `--list` assertion `:229-238` → six template names + two example starters, websocket names asserted ABSENT; `cd rust` `:123`; wasm-rust `:133`; commit msg `:278` → "riz new"; helper `.arg("init")` → "new"), `tests/scaffold_e2e.rs` (typescript-bun, python), `tests/cli_doctor.rs` (typescript-bun + hint string), `tests/lambda_shape_conformance.rs` (scope += `templates/*/`; new test: variant→template map `{Bun:"typescript-bun", Node:"typescript-node", Python:"python", Rust:"rust", Go:"go", Wasm:"wasm-rust"}` is total, 1:1, and every mapped dir exists + BUILTINS contains exactly these six template rows).

- [ ] Steps: update; `cargo nextest run --test cli_init --test scaffold_e2e --test cli_doctor --test lambda_shape_conformance` all green; commit.

### Task 4: PR2 — tests/template_smoke_all.rs (isolated binary)

New isolated binary mirroring e2e_smoke_all's exclusion pattern. Per template: scaffold via the real binary (`riz new <name>` in a temp dir, `RIZ_TEMPLATE_REPO`=repo root), build the compiled legs (rust: `cargo build --release`; go: `go build -o hello .`; wasm-rust: append `[patch."https://github.com/24X7/riz"] riz-wasm = { path = "<repo>/crates/riz-wasm" }` to the scaffolded Cargo.toml, then `cargo build --release --target wasm32-wasip1`), boot `riz run` headless in the scaffold dir on an ephemeral port, GET `/hello?name=alice`, assert 200 + `"hello, alice"` + context fields present (functionName/awsRequestId where the template returns them). Toolchain-missing → per-leg SKIP eprintln (same convention as e2e_smoke_all). CI: ci.yml filter line + new step; CLAUDE.md commands; CONTRIBUTING note; plan/spec test-command references.

- [ ] Steps: write; run `cargo nextest run --test template_smoke_all` (expect 6 legs pass locally — full toolchain verified present); update ci.yml/CLAUDE.md/CONTRIBUTING; commit.

### Task 5: PR2 — docs/web sweep + ship

- [ ] `README.md:20,50`; `docs/CAPABILITY-CARD.md:129` (10-name list → 6 + 2 starters); `web/start.html:70-158`, `web/docs.html:92-104,257`, `web/gateway.html:88`, `web/examples.html:74`, `web/llms.txt:27,31`, `web/.well-known/riz.json:18`; `docs/demo.tape` + `assets/demo.tape:35`; `docs/mcp/getting-started.md:8`; `docs/migrate-from/aws-lambda.md:138`; `docs/plans/v1-roadmap.md` init/template refs (living doc); CHANGELOG new entry. All `riz init` → `riz new`, template names updated, WebSocket-variants clauses dropped (pointer to examples/chat).
- [ ] Full five gates + `cargo nextest run --test template_smoke_all`; push; PR; merge --admin; pull main.

### Task 6: PR3 — examples reshuffle

**Files:** `git mv` examples/lambdas/{echo-bun,echo-node,echo-python,echo-rust,echo-go,echo-wasm,chat-python,chat-rust} → `tests/fixtures/parity/<name>`; `git rm -r` examples/lambdas/{ping,accounts}; re-point `Cargo.toml` members (echo-rust, chat-rust paths) + exclude (echo-wasm path); every riz.*.toml handler path + demo/smoke scripts; `tests/runtime_parity_*`, `tests/wasm_examples.rs`, `tests/e2e_smoke_all.rs`, `tests/lambda_shape_conformance.rs` scope (parity fixtures leave examples scope; tests/fixtures/ remains exempt); echo-python gains the authorizer/invocationCount parity fields; crud-accounts README absorbs accounts' path-params/rawQueryString/cache notes; `examples/README.md` becomes the showcase index (opens with the conformance guarantee); `web/examples.html` "cleanest how-do-I-write-a-handler reference" copy moves to templates framing.

- [ ] Steps: move; sweep references (`rg "examples/lambdas/(echo-|chat-python|chat-rust|ping|accounts)"`); update tests; verify echo-python drift fix against runtime_parity_echo's canonical shape; full gates (parity + e2e green); PR3; merge.

## Self-Review
Covered every PR2/PR3 item in the spec including CI/CLAUDE.md lockstep, riz-wasm publish deferral (git dep + local patch in smoke), and the web sweep; no placeholders — transformation rules are exact and sweep-verified with file:line; names used across tasks match (typescript-bun/typescript-node/python/rust/go/wasm-rust; template_smoke_all; run_new).
