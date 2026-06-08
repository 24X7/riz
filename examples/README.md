# riz Examples

Thirteen example handlers spanning all four runtimes (Bun, Node.js, Python,
Rust), both protocols (HTTP, WebSocket), and the authorizer surface. Each
example has its own README under `examples/lambdas/<name>/`.

## Prerequisites

- [bun](https://bun.sh) on `PATH` (TypeScript handlers)
- [node](https://nodejs.org) on `PATH` (Node.js handlers)
- `python3` on `PATH` (Python handlers)
- riz built: `cargo build --release` (or `cargo build` for dev)
- For the Rust handlers: `cargo build --release --manifest-path examples/lambdas/echo-rust/Cargo.toml`
  and `…/chat-rust/Cargo.toml`
- For the WebSocket smoke tests: [`websocat`](https://github.com/vi/websocat)

## The examples

| Example | Runtime | Protocol | Capability |
|---|---|---|---|
| [`ping`](lambdas/ping/) | Bun | HTTP | Minimal handler, no input |
| [`accounts`](lambdas/accounts/) | Bun | HTTP | Path params + query string |
| [`events`](lambdas/events/) | Bun | HTTP | JSON body + validation |
| [`crud-accounts`](lambdas/crud-accounts/) | Bun | HTTP | All five HTTP verbs |
| [`echo-bun`](lambdas/echo-bun/) | Bun | HTTP | Full event/context surface |
| [`echo-node`](lambdas/echo-node/) | Node.js | HTTP | Same surface, Node.js |
| [`echo-python`](lambdas/echo-python/) | Python | HTTP | Same surface, Python |
| [`echo-rust`](lambdas/echo-rust/) | Rust | HTTP | Same surface, Rust |
| [`chat`](lambdas/chat/) | Bun | WebSocket | WS lifecycle + @connections |
| [`chat-python`](lambdas/chat-python/) | Python | WebSocket | WS, Python |
| [`chat-rust`](lambdas/chat-rust/) | Rust | WebSocket | WS, Rust |
| [`auth-allow-bun`](lambdas/auth-allow-bun/) | Bun | HTTP | REQUEST authorizer (allow) |
| [`auth-deny-bun`](lambdas/auth-deny-bun/) | Bun | HTTP | REQUEST authorizer (deny) |

The `echo-*` set (Bun, Node.js, Python, Rust) and the `chat-*` trio are
**runtime-parity sets**: every implementation emits identical responses,
asserted by `tests/runtime_parity_echo.rs` and the WebSocket integration tests.

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
curl 'http://localhost:3000/accounts/42?include=profile'
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
`file.export` convention (e.g. `./lambdas/ping/index.handler`).
