# riz Examples

Three example Bun lambda handlers demonstrating the core input/output patterns.

## Prerequisites

- [bun](https://bun.sh) installed and on `PATH`
- riz binary built: `cargo build --release` (or `cargo build` for dev)

## Dev Mode

Colorized logs, debug level, TUI dashboard always on, hot-reload of config:

```bash
cargo run -- --dev
```

Loads `examples/riz.dev.toml` by default. All three routes are available:

```bash
# Health check — no input
curl http://localhost:3000/ping

# Path param + query string
curl "http://localhost:3000/accounts/42?include=profile"

# JSON body
curl -X POST http://localhost:3000/events \
  -H "content-type: application/json" \
  -d '{"type":"signup","userId":"abc123"}'
```

### Hot-Reload

While `riz --dev` is running, edit `examples/riz.dev.toml` — change a timeout,
add a route, or remove one. The TUI Routes tab updates within ~200ms without
restarting the host.

## Prod Mode

JSON-structured stdout logs, no TUI, caching enabled on `GET /accounts/:id`:

```bash
cargo run -- --config examples/riz.prod.toml
```

Same curl commands work. The second request to `/accounts/:id` is served from
cache — watch the Cache tab hit count increment if you run with `--config examples/riz.prod.toml`
and a terminal (atty detected).

## Cache Invalidation

```bash
curl -X POST http://localhost:3000/cache/invalidate \
  -H "content-type: application/json" \
  -d '{"prefix":"GET:/accounts/"}'
```

Returns `{"evicted": N}` with the number of entries cleared.

## Lambda Structure

Each lambda is a standard AWS HTTP Gateway v2 handler:

```typescript
export const handler = async (event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ... }),
  };
};
```

riz routes requests to these handlers via stdin/stdout — no SDK changes needed.
The `handler` field in `riz.toml` points directly to the `.ts` file; naming is
entirely up to you.
