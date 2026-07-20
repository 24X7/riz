# CLAUDE.md

riz is a self-hosted AWS Lambda + API Gateway v2 runtime in one Rust binary:
six runtimes (Bun, Node.js, Python, Rust, Go, WASM), every function an MCP tool,
no per-request cold start. Scope is HTTP/WS Lambdas only — SQS/SNS/S3/
EventBridge-style event sources are out of scope by decision, not omission.

## Safety-critical rules (binding)

**[docs/SAFETY.md](docs/SAFETY.md)** — NASA's Power of 10 adapted to Rust —
is binding for all code compiled into the riz binary (`src/`, i.e.
`--lib --bins`). Examples (`examples/lambdas/*`), tests, benches, and fixtures
are exempt by policy.

Working rules that follow from it:

- The enforced lint tier lives in `Cargo.toml` `[workspace.lints]`
  (+ `clippy.toml` thresholds); only the riz package opts in.
- `scripts/safety-check.sh --gate` is a CI gate (prod-only zero-tolerance
  lints, e.g. `clippy::panic`).
- `scripts/safety-check.sh` prints the ratchet report — violation counts may
  only go **down**. Never add a site to a ratchet lint; when a lint's count
  hits zero, promote it (ratchet → gate → enforced) per the protocol in
  docs/SAFETY.md.
- Every `unsafe` block needs `#[allow(unsafe_code)]` at the site plus a
  `// SAFETY:` proof, one operation per block. Any other `#[allow]` of a
  safety lint needs an adjacent justification comment.
- New code in `src/`: no `unwrap`/`expect`/`panic!`/indexing-`[]` on runtime
  data, bounded channels only, loops either provably bounded or matching the
  supervised event-loop contract (docs/SAFETY.md rule 2).

## Build & test gates

Run before every push (CI enforces all of them):

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
./scripts/safety-check.sh --gate
cargo nextest run --workspace -E 'not binary(e2e_smoke_all) and not binary(template_smoke_all) and not binary(perf_regression) and not binary(chaos)'
cargo nextest run --test e2e_smoke_all   # isolated: boots the example fleet
cargo nextest run --test template_smoke_all  # isolated: scaffolds + boots all six templates
cargo nextest run --test perf_regression --no-capture  # isolated: throughput/latency floor + trend
cargo nextest run --test chaos               # isolated: deliberate fault injection
# or the whole flow in one command: scripts/validate.sh
```

- **cargo-nextest only, never `cargo test`** — process-per-test isolation and
  the leak signal (a child process outliving its test fails the run).
- The TUI is driven only by `riz --dev` (flag before the subcommand);
  `riz run` is headless. No `--no-tui`, no TTY detection.

## Layout notes

- Root package = the riz binary; `examples/lambdas/*` are separate workspace
  members (some excluded: wasm targets build via their own manifests).
- `web/` deploys to Vercel on its own; never mix Rust/build code into it.
  The pages use Turbo Drive, which keeps `site.css` across in-page navigations —
  so after editing `site.css`, bump the `?v=` on the stylesheet `<link>` in all
  `web/*.html` (it carries `data-turbo-track="reload"`, forcing a full reload on
  a version change). Skip this and CSS edits render stale until a hard refresh.
- Assessments and design docs live under `docs/` (`assessments/`, `plans/`,
  `specs`-style documents); production bug postmortems in
  `docs/production-bugs.md`.
- Runtime/language support has one source of truth: the `RuntimeKind` enum in
  `src/config.rs`. When you add or remove a runtime — or change a function shape
  (module/export vs. static-binary vs. wasm) — update the count and enumerated
  list everywhere it appears in prose, in lockstep: `Cargo.toml` `description`,
  `README.md`, this file's intro line, `registries/README.md`, `CONTRIBUTING.md`,
  `docs/CAPABILITY-CARD.md`, `server.json`, `examples/README.md`,
  `examples/demo.py`, `templates/`, and `web/`. Keep the wording greppable
  (`rg -n "runtimes"`) so the set never drifts out of sync again.
