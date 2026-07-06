# Contributing to Riz

Practical guide for working on Riz the project — not for using Riz to run your own Lambdas (that's `README.md`).

Every command below has been run on this repo before being documented. The `tests/docs_commands_runnable.rs` guard re-runs the safe, fast CLI commands marked `# @verify` on every `cargo nextest run` and shape-checks the rest, so the docs can't silently rot. If a command stops working, that test fails and CI catches it.

> **Flag ordering matters.** `--config`, `--port`, `--log-level`, and `--dev` are *global* flags on the top-level `riz` command — they go **before** the subcommand. `riz --config foo.toml routes` works; `riz routes --config foo.toml` does **not** parse. Every example below follows the global-flags-first form.

## Toolchain setup

These are one-time, networked installs. They are not executed by the docs guard — only shape-checked — so run them yourself once per machine.

```bash
# Rust (stable channel) — one-time, networked
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Hard rule for this repo: nextest only, never `cargo test`
cargo install cargo-nextest --locked

# Optional but recommended for the inner loop
cargo install cargo-watch --locked
```

Runtime dependencies the tests need (one-time, networked — skip the ones for runtimes you're not touching):

```bash
# Bun (for any TypeScript/JavaScript test) — one-time, networked
curl -fsSL https://bun.sh/install | bash

# python3 (for any Python test) — already on macOS, apt-install on Linux
python3 --version

# wrk (for the benchmark harness) — one-time, networked
brew install wrk     # macOS
# or build from https://github.com/wg/wrk on Linux
```

If you skip any of these, the matching tests skip themselves with a `SKIP:` message rather than failing — so it's safe to work on, say, the Python adapter without Bun installed.

## Inner loop

The fastest "did I break it?" cycle:

```bash
cargo check                                                   # ~40s on a cold machine, fast after that
cargo run -- --dev --config examples/riz.dev.toml run         # boot riz against the example fleet with the live TUI
cargo nextest run -E 'test(my_thing)'                         # run any test whose name matches
```

### Watch mode (re-checks on save)

```bash
cargo watch -x check                                  # re-typecheck on every save
cargo watch -x 'nextest run --test cli_doctor'        # re-run a single test file on save
cargo watch -x 'nextest run -E "test(mcp_inspect)"'   # re-run by name pattern on save
```

### Booting riz against the bundled examples

```bash
# --dev flag: TUI on + debug log level. It does NOT change which config is
# loaded — pass --config explicitly when working inside this repo.
cargo run -- --dev --config examples/riz.dev.toml run

# Without TUI (better for scripting / piping):
cargo run -- --log-level warn --config examples/riz.dev.toml run

# Override the port:
cargo run -- --dev --port 4000 --config examples/riz.dev.toml run

# Crank up the noise on a specific module:
RUST_LOG=riz::process=debug,info cargo run -- --dev --config examples/riz.dev.toml run
```

`--dev` only turns on the TUI and bumps the default log level to `debug`. It does **not** pick a config — the default config path is always `riz.toml` in CWD unless you pass `--config`. Inside this repo there is no top-level `riz.toml`, so always pass `--config examples/riz.dev.toml` to boot against the bundled example fleet.

### Verifying CLI behavior without rebuilding

`cargo run -- <args>` rebuilds incrementally — usually under 5 seconds after the first build. Global flags (`--config`, `--port`, `--log-level`, `--dev`) come before the subcommand:

```bash
# @verify exit=0
cargo run --quiet -- --version
# @verify exit=0
cargo run --quiet -- --config examples/riz.dev.toml routes
# @verify exit=0
cargo run --quiet -- --config examples/riz.dev.toml validate
# @verify exit=0
cargo run --quiet -- --config examples/riz.dev.toml doctor
# @verify exit!=0   (clean connection-refused: nothing is listening on that port)
cargo run --quiet -- mcp inspect --url http://127.0.0.1:59999/_riz/mcp
```

## Testing

**Hard rule: `cargo nextest run`, never `cargo test`.** Stored in repo memory.

```bash
# Full suite (~60s, ~700 tests)
cargo nextest run

# Focused by file (matches by test-binary name)
cargo nextest run --test cli_doctor
cargo nextest run --test scaffold_e2e
cargo nextest run --test landing_page_contract
cargo nextest run --test docs_commands_runnable   # the guard that keeps THIS doc runnable

# Focused by name substring (matches binary name OR test name)
cargo nextest run mcp           # all MCP-related tests
cargo nextest run hotreload     # all hot-reload tests

# Focused by name pattern (nextest filterset language)
cargo nextest run -E 'test(mcp_inspect)'        # any test with "mcp_inspect" in the name
cargo nextest run -E 'test(/spec_2025/)'        # regex match

# Skip slow tests during iteration (then run them before PR)
cargo nextest run --no-fail-fast -E 'not test(/hotreload|deploy|perf/)'
```

### Smoke-testing a change against real handlers

After modifying anything in `src/process/` or `src/runtime/`, the fastest end-to-end check is to boot the example fleet and hit every route. These routes all exist in `examples/riz.dev.toml`:

```bash
# Boot, hit every example route, verify, tear down. (Server-boot + curl +
# trailing `&` — shape-checked by the docs guard, not auto-executed.)
cargo run --quiet -- --log-level warn --config examples/riz.dev.toml run &
RIZ_PID=$!
sleep 3                                                   # let bun/python workers spawn
curl -s http://127.0.0.1:3000/ping
curl -s 'http://127.0.0.1:3000/accounts/42'
curl -s -X POST -d '{"x":1}' http://127.0.0.1:3000/events
curl -s -X POST -d '{"x":1}' http://127.0.0.1:3000/echo-python
cargo run --quiet -- mcp inspect --url http://127.0.0.1:3000/_riz/mcp
kill $RIZ_PID
```

There's also a dedicated `tests/scaffold_e2e.rs` that exercises the full scaffold→boot→curl loop for the templates — run it after touching `templates/` or any runtime adapter:

```bash
cargo nextest run --test scaffold_e2e
```

### End-to-end: every example, every runtime

`examples/smoke-all.sh` is the **assertion harness** that boots the real `riz`
binary against `examples/riz.all.toml` and verifies — with status-code **and**
response-body assertions — that every example handler works together across all
six runtimes (Bun, Node, Python, Rust, Go, WASM), plus the WebSocket round-trips,
REQUEST authorizers, CORS, response cache, the MCP surface, the LLM gateway, and
`/_riz/health` + `/_riz/metrics`. It prints `✓`/`✗` per check and **exits
non-zero on any failure**, so a regression in any example or runtime is caught.

It is wired into nextest via `tests/e2e_smoke_all.rs`, so it runs on the same
`cargo nextest run` / CI path as everything else:

```bash
# Runs automatically as part of the full suite…
cargo nextest run

# …or on its own (skips cleanly if the full toolchain isn't present):
cargo nextest run --test e2e_smoke_all

# Or run the harness directly for the full ✓/✗ report on any port:
PORT=3939 bash examples/smoke-all.sh
```

The harness builds the example artifacts it needs (release `echo-rust` /
`chat-rust`, the `echo-go` binary, and the `echo-wasm` / `orders-wasm` guests),
so it needs the full toolchain: `bun`, `node`, `python3`, `go`, and the
`wasm32-wasip1` rust target. "All examples work together" can't be proven
without them, so a missing toolchain fails loudly rather than passing by skip.

#### Known sharp edge: `allowed_paths` + Linux Landlock

`allowed_paths` becomes a **Linux Landlock** filesystem allowlist applied in the
child's `pre_exec` (`src/process/pool.rs`) — i.e. **before `execve`**. The
allowlist currently contains *only* the configured paths, so it does **not**
grant the program being exec'd: on Linux the child can't `exec` `python3` (a
scripted runtime) or the `riz __wasm-host` binary (a wasm runtime) when those
live outside `allowed_paths`, and `riz run` fails to boot with `EACCES`. macOS
has no Landlock, so this only bites on Linux (which the e2e harness caught).

Until this is fixed, `examples/riz.all.toml` leaves `allowed_paths` off its
scripted/wasm handlers. The proper fix is for the Landlock ruleset to also grant
read+exec on the resolved interpreter/host binary and its shared-library dirs
(without broadening reads to e.g. `/etc`, which the `safety.rs` deny-tests rely
on). The feature is otherwise covered by `tests/telemetry_process_isolation.rs`
and the `process::safety` unit tests.

### Benchmark

The throughput/latency benchmark hammers a release-mode `riz` running a single Bun ping handler with [`wrk`](https://github.com/wg/wrk). Two ways to run it.

**One command (recommended).** `scripts/bench.sh` builds release, boots `riz` headless against a ping config, waits for `/ping` to return 200, warms up, runs `wrk`, and tears the server down on exit:

```bash
./scripts/bench.sh
```

Tunables via env vars; `--tty` wraps `wrk` in `script(1)` for terminal-accurate output:

```bash
PORT=4000 DURATION=10s CONNECTIONS=20 THREADS=4 ./scripts/bench.sh
./scripts/bench.sh --tty
```

It exits non-zero with a clear message if `wrk` or `cargo` isn't installed.

**Raw two-terminal flow** (what `scripts/bench.sh` automates), if you want to drive `wrk` by hand:

```bash
# Terminal 1 — boot release riz against the bench config (concurrency = 20):
cargo build --release
./target/release/riz --log-level warn --config benches/bench-config.toml run

# Terminal 2 — match -c<N> to the pool concurrency:
wrk -t4 -c20 -d20s --latency http://127.0.0.1:3000/ping
```

Methodology and the measured headline number (91,419 req/s · p99 845 µs — Bun ping handler, localhost, concurrency = 20, M-series Mac) live in [`benches/README.md`](benches/README.md). `benches/run-bench.sh` is the older hardcoded variant; prefer `scripts/bench.sh`.

## Where the code lives

```
src/
  main.rs              # Clap subcommand definitions + dispatch
  config.rs            # riz.toml parsing + validation
  server.rs            # axum app + graceful shutdown
  router.rs            # path matching + dispatch (hot path)
  gateway.rs           # AWS API Gateway v2 event/response shapes
  runtime/             # the trait surface that adapters implement
  process/             # process pool, liveness watcher, safety primitives
    bun.rs             # Bun adapter (spawns bun + assets/bun-adapter.mjs)
    python.rs          # Python adapter (spawns python3 + assets/python-adapter.py)
    rust.rs            # Rust adapter (spawns user's pre-built binary)
    safety.rs          # Landlock + rlimits + PR_SET_NO_NEW_PRIVS (pre_exec)
  ws/                  # WebSocket upgrade + connection store + @connections API
  auth/                # bearer-token + JWT + REQUEST authorizer
  cors.rs              # CORS preflight + origin echo
  cache.rs             # auth-aware response cache (moka)
  hotreload.rs         # notify-based watcher for riz.toml + handler sources
  deploy.rs            # POST /_riz/deploy hot-swap from S3
  state.rs             # RizState + per-function counters/latency
  observability/       # isolated __telemetry child + OTLP/HTTP-JSON exporter
  tui/                 # ratatui dashboard
  system/              # /_riz/* endpoints (health, metrics, registry, mcp)
    mcp/               # MCP server (spec 2025-11-25)

assets/                # files embedded into the binary via include_str!
  bun-adapter.mjs      # bun side of the JSON-over-stdin protocol
  python-adapter.py    # python side
  templates/           # 7 `riz init <template>` scaffolds (4 HTTP + 3 WebSocket)

examples/
  riz.dev.toml         # dev config for the bundled example fleet
  lambdas/             # 13 example handlers (HTTP + WebSocket × Bun/Python/Rust)

crates/
  riz-rust-runtime/    # public Rust crate user-facing handlers depend on

tests/                 # integration tests (one file per concern)
  cli_*.rs             # CLI subcommand tests
  middleware_*.rs      # auth/CORS/cache/hot-reload
  runtime_parity_*.rs  # cross-runtime behavioral matrix
  scaffold_e2e.rs      # init → boot → curl regression coverage

scripts/
  bench.sh             # one-command `wrk` flow (build → boot → warm → wrk → teardown)

benches/
  run-bench.sh         # older hardcoded `wrk` variant (prefer scripts/bench.sh)
  bench-config.toml    # ping handler at concurrency=20
  README.md            # benchmark methodology + measured headline number

docs/
  mcp/                 # MCP integration + protocol-support docs
  migrate-from/        # AWS Lambda / LocalStack / SAM Local migration guides
  release.md           # how to cut a release (cargo-dist + tag + push)
  production-bugs.md   # tracker (closed)
```

## Adding things

### A new CLI subcommand

1. Add a variant to `Commands` enum in `src/main.rs` (mirror the `Mcp`, `Init`, `Doctor` patterns)
2. Branch in `main()` to handle it before the config-load path if it doesn't need a riz.toml
3. Add an integration test in `tests/cli_<name>.rs`
4. Surface in README under the relevant section

### A new system endpoint (`/_riz/*`)

1. New module in `src/system/<name>.rs` implementing the `LambdaHandler` trait
2. Register it in `src/main.rs` alongside the other system handlers
3. Add an integration test in `tests/system_functions_integration.rs`
4. Update `web/llms.txt` so MCP clients see the new endpoint

### A new function-runtime adapter

1. New file in `src/process/<lang>.rs` matching `bun.rs` / `python.rs` shape
2. Wire the spawn path in `src/process/mod.rs` (look for `RuntimeKind` match arms)
3. Add the runtime-side adapter (line-delimited JSON over stdin/stdout) in `assets/<lang>-adapter.<ext>`, include_str!-load it in the spawner
4. Add a config variant in `RuntimeKind` (`src/config.rs`)
5. Add a `riz init <lang>-http` template under `templates/<lang>-http/` + register its name in `BUILTINS` in `src/template_fetch.rs` (templates load from git — never embedded)
6. Add a parity test sequence in `tests/runtime_parity_*.rs`
7. Add an e2e test in `tests/scaffold_e2e.rs`
8. Document in the README runtimes list + `docs/migrate-from/aws-lambda.md`

### A new MCP capability

1. Update the version negotiation in `src/system/mcp/protocol.rs` if it requires a newer spec
2. Wire the new method or capability in `src/system/mcp/mod.rs` / `tools.rs`
3. Add regression tests in the `tests` module of `src/system/mcp/mod.rs` (the inner-mod tests have all the setup scaffolding)
4. Update `docs/mcp/protocol-support.md` matrices
5. Update `web/llms.txt` + the website's MCP pill row

## Before opening a PR

```bash
cargo fmt --check
cargo clippy -- -D warnings   # NOTE: some pre-existing warnings exist; clean them up if you touch the file
cargo nextest run             # MUST pass clean
```

If you're touching:
- **The landing page** (`web/index.html`) — `cargo nextest run --test landing_page_contract` must pass
- **Anything in `templates/`** — `cargo nextest run --test scaffold_e2e --test cli_init` must pass
- **Anything in `src/system/mcp/`** — `cargo nextest run mcp` must pass
- **Any runtime adapter** — `cargo nextest run --test runtime_parity_echo` must pass

## Debugging tips

### See what a child process actually does

`assets/bun-adapter.mjs` and `assets/python-adapter.py` log to their own stderr; riz captures and forwards that to its tracing pipeline. To see it:

```bash
RUST_LOG=riz::process=debug cargo run -- --dev --config examples/riz.dev.toml run
```

### Reproduce a single test that's flaky on CI

```bash
cargo nextest run -E 'test(/<failing_test_name>/)' --no-fail-fast --retries 5
```

Most flakes on CI are timing-related — `cargo nextest list-tests` shows the test binary, and you can also run the binary directly:

```bash
./target/debug/deps/<test-binary>-<hash> <test_name> --nocapture
```

### Step through a request in production-like mode

```bash
RUST_LOG=riz=trace cargo run -- --log-level trace --config examples/riz.dev.toml run
# Then in another terminal:
curl -v http://127.0.0.1:3000/<route>
```

The structured JSON logs include the `req` correlation ID so you can grep one request's path through the system.

### Profile a hot path

```bash
cargo install cargo-flamegraph
sudo cargo flamegraph --bin riz -- --log-level warn --config benches/bench-config.toml run
# Hit it with wrk in another terminal, then Ctrl-C
```

(`sudo` is needed for the perf counters on Linux; on macOS use `cargo instruments` instead.)

## Project conventions

- **No `cargo test`** — `cargo nextest run` only. Hard rule.
- **No `unwrap()` in new prod code** — use `anyhow::Context` or proper `Result` propagation. Existing prod `expect()` calls are all infallible-by-construction (see commit history). Test code can use unwrap freely.
- **No `FIXME(wave-*)` comments** — those were a pre-v0.1 convention. If you need a "future work" marker, use `// TODO:` with a clear next-step or open an issue.
- **Comments explain *why*, not *what*.** The CLAUDE.md guidance is the canonical reference.
- **One commit, one concern.** The git history is the project's narrative; PRs that touch 10 unrelated things are hard to review and harder to revert.

## License

Contributions are licensed under MIT, same as the project.
