# osbox — Self-Hosted AWS Lambda Host Design

**Date:** 2026-05-18
**Status:** Approved

## Overview

`osbox` is a Rust CLI binary that serves as a self-hosted replacement for AWS Lambda + HTTP API Gateway (v2). It runs as a single always-on process, routes HTTP requests to language-specific lambda processes via stdin/stdout using the AWS HTTP Gateway v2 JSON format, and provides output caching, Datadog integration, and a live Ratatui TUI dashboard.

**Goals:**
- Drop-in compatibility with existing AWS HTTP Gateway v2 lambda handlers (no handler code changes)
- Eliminate cold starts and API Gateway overhead
- Run on EC2, containers, or bare metal
- Open source — single static binary, easy to install and contribute to

**Non-goals:**
- VPC/IAM/SQS/SNS integration
- AWS console or CloudFormation compatibility
- Multi-tenant isolation (lambdas are trusted, owned by the operator)

---

## Architecture

```
Request
  │
  ▼
┌─────────────────────────────────────────┐
│  axum HTTP server (tokio)               │
│  - Parses incoming HTTP                 │
│  - Builds HTTP Gateway v2 JSON payload  │
└──────────────┬──────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────┐
│  Router                                 │
│  - Matches route → lambda entry point   │
│  - Reads osbox.toml (hot-reloaded)      │
└──────────┬──────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────┐
│  Cache Layer (moka)                     │
│  - TTL check per route config           │
│  - POST /cache/invalidate endpoint      │
│  - Max-size cap to bound memory         │
└──────┬──────────────────────────────────┘
       │ miss                   │ hit
       ▼                        ▼ return cached response
┌─────────────────────────────────────────┐
│  Process Manager                        │
│  - Pool of warm lambda processes        │
│  - stdin → payload, stdout → response   │
│  - Timeout enforcement (per-route)      │
│  - Crash restart + health tracking      │
└──────────────┬──────────────────────────┘
               │ async, non-blocking
               ▼
┌─────────────────────────────────────────┐
│  Metrics Emitter                        │
│  - Buffered DogStatsD / Datadog HTTP    │
│  - Traces, durations, errors, cache     │
└─────────────────────────────────────────┘
```

A **Ratatui TUI** provides a live terminal dashboard: request throughput per route, lambda process health, cache hit/miss ratio, Datadog flush queue depth.

---

## Configuration

Routes and runtimes are defined in `osbox.toml`, tracked in git.

```toml
[server]
port = 3000
host = "0.0.0.0"

[cache]
default_ttl_secs = 0        # disabled by default
max_size_mb = 128

[datadog]
enabled = true
statsd_host = "127.0.0.1:8125"
service = "osbox"
env = "production"

[[routes]]
path = "/auth/signin"
method = "POST"
runtime = "bun"
handler = "./lambdas/signin/index.ts"
timeout_ms = 5000

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./lambdas/accounts/index.ts"
cache_ttl_secs = 30
timeout_ms = 3000

[[routes]]
path = "/api/data"
method = "GET"
runtime = "rust"
handler = "./lambdas/data/target/release/data-lambda"
cache_ttl_secs = 60
timeout_ms = 3000
```

Hot-reload: the router watches `osbox.toml` for changes and reloads routes without restarting the host or dropping in-flight requests.

---

## Lambda Process Contract

All runtimes share the same stdin/stdout protocol:

- **stdin:** single-line JSON, AWS HTTP Gateway v2 payload format
- **stdout:** single-line JSON, AWS HTTP Gateway v2 response format (`statusCode`, `headers`, `body`, `isBase64Encoded`)
- **stderr:** captured and logged by the host; never forwarded to callers
- **lifecycle:** processes are spawned at startup and kept warm; one process per route by default

The host never dynamically links or evaluates lambda code. The process boundary is the isolation boundary.

### Runtime Implementations

Each runtime implements a `LambdaRuntime` trait. Adding a new language is a self-contained addition.

**Phase 1 — Bun/Node:**
```
bun run <handler>
```
Process stays alive, reads one payload per request from stdin, writes one response to stdout. Existing AWS Lambda handlers using the Lambda Runtime SDK work without modification via a thin stdin/stdout adapter (provided as an npm package).

**Phase 2 — Rust:**
```
<compiled-binary>
```
Same stdin/stdout contract. Adapter provided as a Rust crate wrapping the Lambda Runtime SDK.

**Phase 3 — Python:**
```
python <handler>
```
Same contract. Adapter provided as a pip package.

---

## Cache Layer

- **Backend:** `moka` (concurrent, TTL-aware in-memory cache)
- **Key:** `{METHOD}:{path}?{query_string}` — e.g. `GET:/accounts/123?include=profile`. Query string is included verbatim; routes with no query string use `GET:/accounts/123?`.
- **TTL:** per-route `cache_ttl_secs`; 0 = disabled
- **Max size:** configurable `max_size_mb` — evicts LRU when full
- **Invalidation API:**
  ```
  POST /cache/invalidate
  Content-Type: application/json

  // Exact key(s):
  {"keys": ["GET:/accounts/123?", "GET:/api/data?"]}

  // Or by prefix (invalidates all matching):
  {"prefix": "GET:/accounts/"}
  ```
  Returns 200 with count of evicted entries.

---

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Lambda timeout | SIGKILL process, restart, return 504 |
| Lambda crash (non-zero exit) | Restart process, return 502 for in-flight request |
| Malformed JSON from lambda | Return 502, log raw stdout |
| Lambda stderr output | Captured, logged, never forwarded |
| Consecutive crashes (configurable threshold) | Mark route unhealthy, fast-fail with 503 until stable |
| Cache memory cap hit | LRU eviction, no error |

**Host self-protection:**
- The host never panics on bad lambda output
- Each lambda process runs in its own process group; the host can signal the entire group
- Optional per-lambda `ulimit`-style resource caps applied at spawn via `setrlimit`

---

## Datadog Integration

Metrics are emitted asynchronously over DogStatsD (UDP). Phase 1 targets DogStatsD exclusively — it is fire-and-forget UDP, the lowest-overhead path. The metrics path is a non-blocking side channel — a slow or unavailable Datadog agent never affects request latency.

**Metrics emitted:**
- `osbox.request.duration` (histogram, tagged by route, method, status)
- `osbox.request.count` (counter, tagged by route, status)
- `osbox.cache.hit` / `osbox.cache.miss` (counter, tagged by route)
- `osbox.lambda.crash` (counter, tagged by route, runtime)
- `osbox.lambda.timeout` (counter, tagged by route)
- `osbox.lambda.healthy` (gauge, tagged by route — 1/0)

Distributed traces via Datadog APM: the host injects trace context headers into the Gateway v2 payload so lambda handlers can participate in traces.

---

## Ratatui TUI

Split-pane terminal dashboard, always visible when running interactively. Suppressed when stdout is not a TTY (e.g., running as a systemd service or in a container).

**Panes:**
- **Routes** — per-route: RPS, p50/p95 latency, cache hit%, health status
- **Processes** — per lambda: PID, uptime, restart count, memory (RSS)
- **Cache** — total entries, memory used, global hit rate
- **Logs** — last N log lines (errors and warnings), scrollable

---

## CLI Interface (Clap)

```
osbox [OPTIONS] [COMMAND]

Commands:
  start     Start the host (default if no command given)
  validate  Validate osbox.toml without starting
  routes    Print resolved route table
  deploy    Trigger a deploy manually (calls local deploy API)

Options:
  -c, --config <FILE>    Config file [default: ./osbox.toml]
  -p, --port <PORT>      Override server port
      --no-tui           Disable TUI, log to stdout as JSON
      --log-level <LVL>  trace|debug|info|warn|error [default: info]
```

---

## Lambda Deployment

Lambdas are deployed via a two-step flow: artifact upload to S3, then a deploy API trigger. The host never polls S3 — the API call is the sole trigger.

### Flow

1. CI/CD builds and zips the lambda (`signin-v2.zip`)
2. CI/CD uploads zip to a configured S3 bucket/key
3. CI/CD calls `POST /deploy` with the lambda name and S3 path
4. Host downloads and unpacks the zip to a staging directory
5. Host drains in-flight requests to the old process (configurable drain timeout, default 10s)
6. Host swaps to the new process, SIGTERMs the old one (SIGKILL after drain timeout)
7. Deploy API returns 200 once the new process is healthy

### Deploy API

```
POST /deploy
Authorization: Bearer <token>
Content-Type: application/json

{
  "lambda": "signin",
  "s3_bucket": "my-deploys",
  "s3_key": "lambdas/signin-v2.zip"
}
```

Response:
```json
{"status": "ok", "lambda": "signin", "pid": 12345}
```

On failure (download error, process won't start, health check timeout): returns 500 with error detail, old process remains active.

### Security

- **Bearer token** — required on all `/deploy` requests. Defined in `osbox.toml` under `[deploy]` or overridden via `OSBOX_DEPLOY_KEY` env var. Env var takes precedence.
- **IP allowlist** — optional list of CIDR ranges in `[deploy]` config. Requests from unlisted IPs are rejected with 403 before token validation.
- HTTPS is the responsibility of a reverse proxy (nginx, caddy) in front of osbox. The deploy endpoint binds to the same port as the main server.

```toml
[deploy]
# deploy_key = "..."        # prefer OSBOX_DEPLOY_KEY env var
allowed_cidrs = ["10.0.0.0/8", "172.16.0.0/12"]  # optional

[aws]
region = "us-east-1"
# Credentials via standard AWS env vars or instance profile
```

### Zip Structure

The zip must contain the handler entry point at its root or a known path matching the route's `handler` field. No required directory structure beyond that — whatever the handler config points to.

### Rollback

Rollback is a redeploy of the previous artifact. No automatic rollback — the operator re-triggers deploy with the prior S3 key.

---

## Phased Delivery

| Phase | Scope |
|-------|-------|
| **1** | Rust host binary, axum HTTP server, TOML config + hot-reload, router, process manager (Bun/Node runtime), TTL cache + invalidation API, deploy API (S3 + bearer token + IP allowlist), Ratatui TUI, Datadog integration, Clap CLI |
| **2** | Rust lambda runtime + adapter crate |
| **3** | Python lambda runtime + adapter package |

Each phase is additive. Phase 1 is production-ready standalone. Phases 2 and 3 add runtime support without breaking the host interface.

---

## Key Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| HTTP server | `axum` |
| CLI args | `clap` |
| TUI | `ratatui` |
| In-memory cache | `moka` |
| Config parsing | `toml` + `serde` |
| Config hot-reload | `notify` |
| Metrics (StatsD) | `dogstatsd` or `cadence` |
| Serialization | `serde_json` |
| Logging | `tracing` + `tracing-subscriber` |
