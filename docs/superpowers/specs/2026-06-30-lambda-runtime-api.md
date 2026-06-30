# AWS Lambda Runtime API — run UNMODIFIED compiled binaries (design)

Status: **IN PROGRESS** (approved direction; building).
Last updated: 2026-06-30.

## Goal

Run an **unmodified** official AWS Lambda **compiled** binary (Go built with
`github.com/aws/aws-lambda-go`'s `lambda.Start`, Rust built with
`lambda_runtime::run` / cargo-lambda, or any `provided.al2023` custom runtime)
on riz with **zero riz library and zero code changes**. One sentence: *riz speaks
the real AWS Lambda Runtime API, so the official runtime clients connect to it
exactly as they connect to AWS.*

## Why (the problem)

Compiled AWS runtimes don't read events from a generic place — the binary links
a runtime client that speaks the **Lambda Runtime API**: it polls
`GET $AWS_LAMBDA_RUNTIME_API/2018-06-01/runtime/invocation/next` over HTTP and
POSTs results back. riz's current process transport is its own stdin/stdout
line-JSON envelope, so an official binary can't talk to it — hence the
`riz-rust-runtime` / `riz-go-runtime` helpers, which force a code change
(`lambda.Start` → `riz.Start`). That defeats the "no changes" goal.

Scripted runtimes (bun/node/python) already require no user change — riz's
embedded adapter calls the user's exported `handler`. Only **compiled** runtimes
need this fix.

## The Runtime API contract (verified against AWS docs, version 2018-06-01)

- Endpoint from env `AWS_LAMBDA_RUNTIME_API` = `host:port` (no scheme).
- `GET /2018-06-01/runtime/invocation/next` → 200, body = event JSON. Response
  headers: `Lambda-Runtime-Aws-Request-Id`, `Lambda-Runtime-Deadline-Ms`
  (unix ms), `Lambda-Runtime-Invoked-Function-Arn`, plus optional
  `Lambda-Runtime-Trace-Id` / `-Client-Context` / `-Cognito-Identity`. **Long-poll
  — never time out the GET.**
- `POST /2018-06-01/runtime/invocation/{requestId}/response` body = response → 202.
- `POST /2018-06-01/runtime/invocation/{requestId}/error` body = error JSON,
  header `Lambda-Runtime-Function-Error-Type` → 202.
- `POST /2018-06-01/runtime/init/error` → 202 (log it).

## Architecture

**One Runtime-API endpoint per worker slot.** Each riz worker = one AWS
"execution environment" = one in-flight invocation at a time. The official
clients carry no worker id, so each worker must connect to its **own**
`host:port`. riz binds a `127.0.0.1:0` listener per worker slot and sets that
worker's `AWS_LAMBDA_RUNTIME_API` to it. The endpoint is tied to the **slot**, not
the child — when a child crashes and respawns, it reconnects to the same port.

```
unmodified handler binary (aws-lambda-go / lambda_runtime)
   GET  /…/invocation/next      ←─ riz hands it the queued event + headers
   POST /…/invocation/{id}/response ─→ riz returns it to the HTTP caller
        │
   per-slot riz Runtime-API endpoint (127.0.0.1:PORT, AWS_LAMBDA_RUNTIME_API)
        │  mpsc<Invocation> in, oneshot<Result> back
   riz warm worker pool  ← unchanged dispatch/semaphore/liveness/safety
```

### New module: `src/process/runtime_api.rs`
- `struct Invocation { request_id, deadline_ms, arn, event: Vec<u8>, respond: oneshot::Sender<Result<Vec<u8>, String>> }`.
- `WorkerEndpoint`: binds `127.0.0.1:0`, owns `invoke_rx: mpsc::Receiver<Invocation>`
  + `pending: Mutex<HashMap<String, oneshot::Sender<..>>>`. Serves the four routes
  via a tiny axum app (axum 0.7 is already a dep). `next` awaits `invoke_rx.recv()`,
  records `request_id → respond`, returns body+headers. `response/{id}` /
  `error/{id}` resolve the oneshot. Returns the bound `SocketAddr` + the
  `mpsc::Sender<Invocation>` for the pool.

### Pool changes (`src/process/pool.rs`, `mod.rs`)
- `LambdaRuntime::transport() -> Transport { Stdio | RuntimeApi }`. `static_binary`
  → `RuntimeApi`; bun/node/python → `Stdio`.
- `ProcessHandle` becomes a transport enum: `Stdio { stdin, stdout }` (today) or
  `RuntimeApi { to_worker: mpsc::Sender<Invocation>, api_addr }` (the endpoint task
  runs independently, persisting across child respawns).
- `spawn_process`: for `RuntimeApi`, provision/reuse the slot endpoint, set
  `AWS_LAMBDA_RUNTIME_API` on the `Command`, spawn the child (stdin/stdout null;
  stderr still captured), keep the safety pre_exec + kill_on_drop.
- `ProcessManager::invoke`/`invoke_generic`: branch on transport. RuntimeApi path:
  build `Invocation` (uuid request id, `deadline_ms = now + timeout`, synthetic
  ARN), `to_worker.send`, await the oneshot under the same `timeout(...)` +
  failure→kill+respawn handling that exists today.

### Spawn env / examples / templates
- `static_binary` runtime no longer relies on stdin/stdout for the event; it just
  execs the binary (the pool injects `AWS_LAMBDA_RUNTIME_API`).
- **Drop** `crates/riz-rust-runtime` and the unused `crates/riz-go-runtime`.
- Migrate to the **official** clients (proves the contract):
  - `examples/lambdas/echo-rust`, `chat-rust` → `lambda_runtime` + `aws_lambda_events`.
  - `templates/rust-http`, `rust-websocket` → `lambda_runtime`.
  - `examples/lambdas/echo-go` (+ a `templates/go-http`) → `aws-lambda-go` `lambda.Start`.
- Update the Cargo workspace `members` (remove riz-rust-runtime) and
  `tests/wave_6_acceptance.rs` reference.

## Tests (hold the line)
- `runtime_api.rs` units: `next` delivers the queued event + headers; `response`
  resolves the oneshot; `error` surfaces an error; unknown id is a no-op 202.
- `tests/runtime_parity_echo.rs`: a **Go** leg (`echo-go`, official aws-lambda-go)
  and the existing **Rust** leg (now official `lambda_runtime`) must emit the
  canonical shape — proving an UNMODIFIED official binary runs.
- `tests/runtime_api_unmodified.rs` (keystone): boot riz with the official
  echo-go/echo-rust binary and assert a real `GET /echo` round-trips — no riz
  import anywhere in the handler.
- CI (`ci.yml`): `setup-go` + `go build` the echo-go fixture; the existing
  `cargo build --release -p echo-rust` continues to build the Rust leg.

## Non-goals (v1)
- X-Ray trace propagation, client-context, cognito identity headers (optional,
  emit empty/none).
- Streaming responses (`/response` with `Lambda-Runtime-Function-Response-Mode`).
- Extensions API, Telemetry API, `AWS_LAMBDA_MAX_CONCURRENCY` concurrent `/next`
  (riz keeps one in-flight per worker; concurrency = pool size, as today).

## Sequencing
1. `runtime_api.rs` module + unit tests.
2. Pool integration (transport enum + invoke branch + spawn env).
3. Migrate echo-rust to official `lambda_runtime`; prove parity; then echo-go
   (official aws-lambda-go) + the unmodified keystone test.
4. Migrate chat-rust + rust/go templates; drop helper crates; fix workspace + CI.
5. Docs (5→6 runtimes, the "no riz library" story) + Vercel artifacts.
