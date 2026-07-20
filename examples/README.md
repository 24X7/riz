# riz Examples

**Templates start you fast; starters are full apps; examples are proof.** Every
handler here is authored as a pure AWS Lambda handler — event in, response out,
never an event loop — and `tests/lambda_shape_conformance.rs` enforces that
statically for every file in this tree. Three tiers, and which one a scaffold is
is a *declared* property (`BuiltinKind`), not a guess from its directory:

- **Templates** (`templates/`) — one minimal per-runtime skeleton, the fast
  start for a language: `riz new <lang>`.
- **Starters** (`ai-chat`, `typescript-todo`) — full-stack app skeletons you
  scaffold and build on: `riz new ai-chat`. They live under `examples/` yet are
  as scaffoldable as any template — `riz new --list` shows both.
- **Examples** (`lambdas/`) — showcase handlers you read, boot, and steal from.
  Not scaffold sources; they are proof, booted by `tests/e2e_smoke_all.rs`.

## Prerequisites

- [bun](https://bun.sh) on `PATH` (TypeScript handlers)
- riz built: `cargo build --release` (or `cargo build` for dev)
- For `orders-wasm`: `rustup target add wasm32-wasip1`, then
  `cargo build --release --target wasm32-wasip1 --manifest-path examples/lambdas/orders-wasm/Cargo.toml`
- For the WebSocket smoke tests: [`websocat`](https://github.com/vi/websocat)

## The showcase

| Example | Runtime | Protocol | Shows off |
|---|---|---|---|
| [`agent-tools`](lambdas/agent-tools/) | Bun | HTTP | Every function is an MCP tool — three tools from one module (multi-export routing) |
| [`crud-accounts`](lambdas/crud-accounts/) | Bun | HTTP | REST over all five verbs, method dispatch, in-memory store |
| [`chat`](lambdas/chat/) | Bun | WebSocket | $connect/$disconnect/$default + @connections push |
| [`orders-wasm`](lambdas/orders-wasm/) | WASM | HTTP | Real deterministic compute on the riz-wasm shim inside the WASI sandbox |
| [`events`](lambdas/events/) | Bun | HTTP | JSON body + validation; the protected target behind the authorizer demos |
| [`auth-allow-bun`](lambdas/auth-allow-bun/) | Bun | HTTP | REQUEST authorizer — the APIGW simple-response contract (allow) |
| [`auth-deny-bun`](lambdas/auth-deny-bun/) | Bun | HTTP | REQUEST authorizer — the 401 deny path |

Two full-stack example starters live one level up — [`../ai-chat`](../ai-chat)
(React chat UI + Bun agent loop through the LLM gateway) and
[`../typescript-todo`](../typescript-todo) (Bun API + React/Vite client) —
both scaffoldable via `riz new`.

The cross-runtime **parity fixtures** (`echo-*` in all six runtimes, the WS
`chat-python`/`chat-rust` mirrors) are test infrastructure, not teaching
material; they live in `tests/fixtures/parity/` and back
`tests/runtime_parity_*.rs`.

## Config files

riz always loads `./riz.toml` unless you pass `--config` explicitly. `--dev` is
a UX flag (TUI + debug logs) — it does **not** change which config is loaded.
So inside this repo you always pass `--config examples/<file>`:

| File | Purpose |
|---|---|
| `riz.all.toml` | Every example above, wired in one config. The completeness reference. |
| `riz.dev.toml` | A focused subset for the dev-loop walkthrough below. |
| `riz.prod.toml` | Prod-style: JSON logs, caching enabled. |

## Run everything at once

```bash
cargo build --release
./examples/smoke-all.sh
```

`smoke-all.sh` boots riz with `riz.all.toml`, exercises every route (HTTP, WS,
authorizers), prints `riz mcp inspect` output and `/_riz/health`, then tears the
server down. It is the single command that proves every example works.

## Dev mode

Colorized debug logs, TUI dashboard, hot-reload of config:

```bash
cargo run -- --dev --config examples/riz.dev.toml run
```

The TUI is shown because of `--dev` and only because of `--dev` — there is no
TTY auto-detection. `riz run` without `--dev` is always headless.

Routes in `riz.dev.toml`:

```bash
curl http://localhost:3000/ping
curl -X POST -H 'content-type: application/json' -d '{"name":"alice"}' http://localhost:3000/accounts
curl -X POST -H 'content-type: application/json' \
  -d '{"type":"signup","userId":"abc123"}' http://localhost:3000/events
```

### Hot-reload

While the dev server runs, edit `examples/riz.dev.toml` — change a timeout, add
or remove a route. The TUI Routes tab updates within ~200ms without restarting
the host.

## Prod mode

JSON-structured stdout logs, no TUI (no `--dev`), caching enabled:

```bash
cargo run -- --config examples/riz.prod.toml run
```

## Cache invalidation

The runtime exposes a built-in cache-invalidation endpoint:

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"prefix":"GET:/accounts/"}' http://localhost:3000/cache/invalidate
# {"evicted": N}
```

## Lambda structure

Every handler is a standard AWS HTTP API Gateway v2 handler — no SDK changes:

```typescript
export const handler = async (event: any, _ctx: any) => ({
  statusCode: 200,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ /* ... */ }),
});
```

The `handler` field in `riz.toml` points at the file + export using the AWS
`file.export` convention (e.g. `./lambdas/events/index.handler`).
