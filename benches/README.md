# Riz benchmarks

Reproducible perf snapshot. Numbers below come from `wrk` against a release-mode `riz` running a minimal Bun ping handler.

## Headline number (v0.1, single Bun handler, 20-process pool)

```
91,419 req/s sustained
   p50 = 152 µs
   p75 = 185 µs
   p90 = 240 µs
   p99 = 845 µs   ← sub-millisecond at the tail
```

Tested on: Apple M-series, 1 host process + 20 Bun worker processes, localhost loopback. The handler is `examples/lambdas/ping/index.handler` — returns `{"status":"ok","ts":<unix>}`.

## Reproducing

The canonical one-command path is **`scripts/bench.sh`** at the repo root — it builds release, boots riz headless against the ping config, waits for `/ping` to return 200, warms up, runs `wrk`, and tears the server down on exit:

```bash
./scripts/bench.sh
# tunables: PORT, DURATION, CONNECTIONS, THREADS (env); --tty for pty-accurate output
PORT=4000 DURATION=10s CONNECTIONS=20 THREADS=4 ./scripts/bench.sh
```

If you'd rather drive `wrk` by hand (note: global flags like `--config`/`--log-level` come **before** the `run` subcommand):

```bash
# 1. Build release binary
cargo build --release

# 2. Start riz with the bench config (concurrency = 20)
./target/release/riz --log-level warn \
    --config benches/bench-config.toml run

# 3. In another terminal, hammer with wrk
wrk -t4 -c20 -d20s --latency http://127.0.0.1:3000/ping
```

Match `wrk -c<N>` to your `concurrency` setting in `bench-config.toml`. Over-saturating the pool will queue requests and inflate the tail; under-saturating leaves processes idle.

`benches/run-bench.sh` is the older hardcoded variant (fixed port 3000, 30s, no warmup). Prefer `scripts/bench.sh`.

## What the numbers mean

- **91k req/s** is the per-host ceiling for THIS handler (a near-empty Bun function). Heavier handlers throttle proportionally; the dispatch layer in Riz adds <200µs.
- **p99 < 1ms** is the cost of the dispatch layer + a single round-trip to a warm Bun worker over stdin/stdout, *not* the cold-start cost. Cold starts (first request after process spawn) are typically 30–60ms for Bun, ~80ms for Python, ~5ms for Rust.
- The dev-mode benchmark (with `concurrency = 1`) bottlenecks immediately — that's expected. Use the dedicated bench config above for perf testing.

## What this benchmark does NOT measure

- **Cold starts** under load (process-pool ramp-up). Future work.
- **WebSocket throughput** (different code path). Future work.
- **MCP `/tools/call`** latency (adds a JSON-RPC envelope + router re-entry). Roughly the same shape as HTTP dispatch + 50–100µs envelope cost.
- **Comparison numbers vs LocalStack / SAM Local.** Both run handlers in Docker containers, so the meaningful number would be "first request after `localstack lambda invoke`" — typically seconds. Riz's first request after `riz run` is ~50ms (Bun cold start), so the dev-loop delta is roughly 100×.

## Caveats

- Loopback latency on real metal will look worse than localhost. Real-world p99 is dominated by network + handler work; the dispatch tax stays near constant.
- These are single-host numbers. Riz is single-tenant single-node by design in v0.1; horizontal scale is a reverse-proxy-in-front concern.
- wrk's "Socket errors: read N" lines are typical keep-alive recycling, not handler failures. The "Requests/sec" line is the honest throughput.
