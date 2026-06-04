# Contributing to Riz

Practical guide for working on Riz the project — not for using Riz to run your own Lambdas (that's `README.md`).

Every command below has been run on this repo before being documented. If a command stops working, the corresponding test will fail and CI will catch it.

## Toolchain setup

```bash
# Rust (stable channel)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Hard rule for this repo: nextest only, never `cargo test`
cargo install cargo-nextest --locked

# Optional but recommended for the inner loop
cargo install cargo-watch --locked
```

Runtime dependencies the tests need (skip the ones for runtimes you're not touching):

```bash
# Bun (for any TypeScript/JavaScript test)
curl -fsSL https://bun.sh/install | bash

# python3 (for any Python test) — already on macOS, apt-install on Linux
python3 --version

# wrk (for the benchmark harness)
brew install wrk     # macOS
# or build from https://github.com/wg/wrk on Linux
```

If you skip any of these, the matching tests skip themselves with a `SKIP:` message rather than failing — so it's safe to work on, say, the Python adapter without Bun installed.

## Inner loop

The fastest "did I break it?" cycle:

```bash
cargo check                              # ~40s on a cold machine, fast after that
cargo run -- --dev run                   # boot riz against examples/riz.dev.toml with the live TUI
cargo nextest run -E 'test(my_thing)'    # run any test whose name matches
```

### Watch mode (re-checks on save)

```bash
cargo watch -x check                                # re-typecheck on every save
cargo watch -x 'nextest run --test cli_doctor'      # re-run a single test file on save
cargo watch -x 'nextest run -E "test(mcp_inspect)"' # re-run by name pattern on save
```

### Booting riz against the bundled examples

```bash
# --dev flag: auto-uses examples/riz.dev.toml + colorized logs + debug level + TUI on
cargo run -- --dev run

# Without TUI (better for scripting / piping):
cargo run -- --log-level warn --config examples/riz.dev.toml run

# Override the port:
cargo run -- --dev --port 4000 run

# Crank up the noise on a specific module:
RUST_LOG=riz::process=debug,info cargo run -- --dev run
```

`--dev` defaults the config path to `examples/riz.dev.toml` and the log level to `debug`. Without `--dev`, the default config is `riz.toml` in CWD.

### Verifying CLI behavior without rebuilding

`cargo run -- <args>` rebuilds incrementally — usually under 5 seconds after the first build:

```bash
cargo run --quiet -- --version
cargo run --quiet -- routes --config examples/riz.dev.toml
cargo run --quiet -- mcp inspect --url http://127.0.0.1:3000/_riz/mcp
cargo run --quiet -- doctor
```

## Running tests

**Hard rule: `cargo nextest`, never `cargo test`.** Stored in repo memory.

```bash
# Full suite (~60s, ~700 tests)
cargo nextest run

# Single test file (matches by binary name)
cargo nextest run --test cli_doctor
cargo nextest run --test scaffold_e2e
cargo nextest run --test landing_page_contract

# By name substring (matches binary name OR test name)
cargo nextest run mcp           # all MCP-related tests
cargo nextest run hotreload     # all hot-reload tests

# By name pattern (nextest filterset language)
cargo nextest run -E 'test(mcp_inspect)'        # any test with "mcp_inspect" in the name
cargo nextest run -E 'test(/spec_2025/)'        # regex match

# Skip slow tests during iteration (then run them before PR)
cargo nextest run --no-fail-fast -E 'not test(/hotreload|deploy|perf/)'
```

### Smoke-testing a change against real handlers

After modifying anything in `src/process/` or `src/runtime/`, the fastest end-to-end check is:

```bash
# Boot, hit every example route, verify, tear down
cargo run --quiet -- --log-level warn --config examples/riz.dev.toml run &
RIZ_PID=$!
sleep 3                                                   # let bun/python workers spawn
curl -s http://127.0.0.1:3000/ping
curl -s 'http://127.0.0.1:3000/accounts/42'
curl -s -X POST -d '{"x":1}' http://127.0.0.1:3000/events
curl -s -X POST -d '{"x":1}' http://127.0.0.1:3000/echo-python
cargo run --quiet -- mcp inspect
kill $RIZ_PID
```

There's also a dedicated `tests/scaffold_e2e.rs` that exercises the full scaffold→boot→curl loop for the templates — run it after touching `assets/templates/` or any runtime adapter.

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
  metrics.rs           # Datadog cadence emitter
  tui/                 # ratatui dashboard
  system/              # /_riz/* endpoints (health, metrics, registry, mcp)
    mcp/               # MCP server (spec 2025-11-25)

assets/                # files embedded into the binary via include_str!
  bun-adapter.mjs      # bun side of the JSON-over-stdin protocol
  python-adapter.py    # python side
  templates/           # 6 `riz init <template>` scaffolds

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

benches/
  run-bench.sh         # `wrk` against a release riz
  bench-config.toml    # ping handler at concurrency=20

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
5. Add a `riz init <lang>-http` template in `assets/templates/` + register in `template_files()` in `src/main.rs`
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
- **Anything in `assets/templates/`** — `cargo nextest run --test scaffold_e2e` must pass
- **Anything in `src/system/mcp/`** — `cargo nextest run mcp` must pass
- **Any runtime adapter** — `cargo nextest run --test runtime_parity_echo` must pass

## Debugging tips

### See what a child process actually does

`assets/bun-adapter.mjs` and `assets/python-adapter.py` log to their own stderr; riz captures and forwards that to its tracing pipeline. To see it:

```bash
RUST_LOG=riz::process=debug cargo run -- --dev run
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
RUST_LOG=riz=trace cargo run -- --log-level trace run
# Then in another terminal:
curl -v http://127.0.0.1:3000/<route>
```

The structured JSON logs include the `req` correlation ID so you can grep one request's path through the system.

### Profile a hot path

```bash
cargo install cargo-flamegraph
sudo cargo flamegraph --bin riz -- --log-level warn run
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
