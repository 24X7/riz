# osbox — Examples Folder and Dev Mode Design

**Date:** 2026-05-20
**Status:** Approved

## Overview

Add an `examples/` folder to the repo with three realistic lambda handlers covering the core input patterns (no input, path param + query string, JSON body). Add a `--dev` flag to the CLI that switches the host into developer mode: colorized logs, debug level, TUI always on, and `osbox.dev.toml` as the default config. Without `--dev`, the host runs in production mode: JSON-structured stdout logs, TUI off.

**Goals:**
- Let the operator run osbox locally and immediately see a working example
- Demonstrate that lambda naming and routing are TOML-driven, not prescribed by osbox
- Show the dev/prod behavioral split clearly at the terminal
- Make config hot-reload visible in the TUI during local development

**Non-goals:**
- Lambda file watching / automatic process restart on `.ts` file changes (Phase 2)
- `osbox dev` as a separate subcommand
- Any change to the lambda stdin/stdout protocol

---

## Examples Folder Structure

```
examples/
  lambdas/
    ping/
      index.ts        — GET /ping, no input, returns {"status":"ok","ts":<epoch ms>}
    accounts/
      index.ts        — GET /accounts/:id?include=..., path param + query string, returns mock account JSON
    events/
      index.ts        — POST /events, JSON body in, echoes payload with confirmed timestamp
  osbox.dev.toml      — dev config: debug logs, no cache, short timeouts, routes to examples/lambdas/
  osbox.prod.toml     — prod config: info logs, caching on accounts, longer timeouts
  README.md           — how to run dev and prod modes
```

### Lambda Contracts

All three lambdas receive the standard AWS HTTP Gateway v2 event and return the standard response shape. No osbox-specific API. Existing Lambda SDK handlers work without modification.

**`ping/index.ts`**
- Method: GET
- Input: none (ignores body and query string)
- Output: `{"status":"ok","ts":<Date.now()>}`
- Purpose: smoke test, demonstrates minimum viable handler

**`accounts/index.ts`**
- Method: GET
- Input: `event.pathParameters.id` (from `:id` path segment), `event.rawQueryString` (e.g. `include=profile`)
- Output: mock account object `{"id":"<id>","name":"...","plan":"...","include":"<param or null>","ts":<epoch>}`
- Purpose: demonstrates path params + query string parsing together

**`events/index.ts`**
- Method: POST
- Input: `JSON.parse(event.body)` — any JSON object
- Output: `{"received":<input payload>,"confirmedAt":<epoch>}`
- Purpose: demonstrates full JSON round-trip; errors on non-JSON body with 400

---

## TOML Configs

### `examples/osbox.dev.toml`

```toml
[server]
port = 3000
host = "127.0.0.1"

[cache]
default_ttl_secs = 0
max_size_mb = 128

[datadog]
enabled = false

[[routes]]
path = "/ping"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/ping/index.ts"
timeout_ms = 1000
concurrency = 1

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/accounts/index.ts"
timeout_ms = 1000
concurrency = 1

[[routes]]
path = "/events"
method = "POST"
runtime = "bun"
handler = "./examples/lambdas/events/index.ts"
timeout_ms = 1000
concurrency = 1
```

No caching — every request hits the lambda. Short 1000ms timeouts so timeout behavior is easy to trigger. Binds to `127.0.0.1` only (not exposed on the network).

### `examples/osbox.prod.toml`

```toml
[server]
port = 3000
host = "0.0.0.0"

[cache]
default_ttl_secs = 0
max_size_mb = 128

[datadog]
enabled = false
statsd_host = "127.0.0.1:8125"
service = "osbox"
env = "production"

[deploy]
# deploy_key = "changeme"   # prefer OSBOX_DEPLOY_KEY env var
allowed_cidrs = []

[aws]
region = "us-east-1"

[[routes]]
path = "/ping"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/ping/index.ts"
timeout_ms = 5000
concurrency = 2

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/accounts/index.ts"
cache_ttl_secs = 30
timeout_ms = 5000
concurrency = 2

[[routes]]
path = "/events"
method = "POST"
runtime = "bun"
handler = "./examples/lambdas/events/index.ts"
timeout_ms = 5000
concurrency = 2
```

`GET /accounts/:id` is cached for 30s — hit/miss counters visible in TUI Cache tab. Concurrency 2 per route. `host = "0.0.0.0"` to accept external connections.

---

## `--dev` Flag

### CLI Change

`--dev` is added as a top-level boolean flag on the `start` subcommand (and the implicit default when no subcommand is given), alongside `--config`, `--port`, `--no-tui`, and `--log-level`.

```
osbox [OPTIONS]

Options:
  --dev              Developer mode: colorized logs, debug level, TUI on, default config osbox.dev.toml
  -c, --config FILE  Config file [default: osbox.dev.toml in --dev, osbox.toml otherwise]
  -p, --port PORT    Override server port
      --no-tui       Disable TUI (ignored in --dev)
      --log-level    trace|debug|info|warn|error [overrides --dev default of debug]
```

### Dev Mode Behavior (`--dev`)

| Concern | Behavior |
|---------|----------|
| Default config | `osbox.dev.toml` (if `--config` not set) |
| Log format | Colorized pretty-print (current behavior) |
| Log level | `debug` (unless `--log-level` overrides) |
| TUI | Always on — `--no-tui` and atty check both ignored |
| Startup banner | Log line: `osbox starting in [dev] mode` |

### Prod Mode Behavior (no `--dev`)

| Concern | Behavior |
|---------|----------|
| Default config | `osbox.toml` |
| Log format | JSON to stdout — one object per line, no ANSI codes, compatible with Datadog/CloudWatch |
| Log level | `info` (unless `--log-level` overrides) |
| TUI | Off — atty check still applies but JSON logs and TUI cannot share the same terminal; TUI is suppressed |
| Startup banner | JSON log line with `mode: "production"` field |

### Implementation

`tracing_subscriber` is initialized differently based on the flag:

- **Dev:** `tracing_subscriber::fmt().pretty().with_env_filter(...)` — current behavior
- **Prod:** `tracing_subscriber::fmt().json().with_env_filter(...)` — structured JSON per line

The `--dev` flag is resolved before config loading so the correct default config path is known. If the default config file doesn't exist, osbox exits with a clear error naming the expected file.

---

## Hot-Reload in Dev

Hot-reload of `osbox.dev.toml` is already implemented. In dev mode the experience is:

1. Run `osbox --dev`
2. TUI appears with Routes tab showing `GET /ping`, `GET /accounts/:id`, `POST /events`
3. Edit `examples/osbox.dev.toml` — add a route, change a timeout, disable a route
4. Within ~200ms the TUI Routes tab updates without restarting the host or dropping in-flight requests

This makes the TOML-as-source-of-truth contract immediately tangible.

---

## README

`examples/README.md` covers:
- Prerequisites: `bun` installed, `cargo build --release` done
- Dev mode: `osbox --dev`
- Prod mode: `osbox --config examples/osbox.prod.toml`
- Three `curl` examples hitting each route
- One `curl` showing cache invalidation via `POST /cache/invalidate`
- Note on editing `osbox.dev.toml` to see hot-reload live
