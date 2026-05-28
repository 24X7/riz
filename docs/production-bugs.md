# riz Production Bug Tracker

Findings from the production-readiness audit (2026-05-22). Ordered by severity.

---

## P0 — Silent data corruption (fix before any prod traffic)

### BUG-01: Pipe desync on non-JSON lambda output ✅ RESOLVED 2026-05-28
**File:** `src/process/mod.rs` — `invoke()` parse arm (~line 147)
**Problem:** When `serde_json::from_str(line.trim())` fails (bad stdout line from lambda), the process is neither killed nor respawned. The pipe is left in an unknown read position. Every subsequent request on that PID gets the *previous* request's response. Silent cross-request data leak.
**Root cause:** `Ok(Ok(line))` arm maps parse errors to `Err(anyhow::Error)` and returns — no process kill.
**Fix:** On any parse failure: kill the process group, respawn, then return 502. Never trust the pipe after a failed exchange.
**Resolved:** fix lives at `src/process/mod.rs:258-271` (`invoke`) and `src/process/mod.rs:386-399` (`invoke_generic`); both parse-failure arms call `handle_process_failure` (`src/process/liveness.rs:11-34`) which `kill_process_group`s the bad PID and respawns. Regression gate: `tests/bug_01_parse_failure_respawns.rs::parse_failure_kills_and_respawns_the_process` spawns a real subprocess that emits non-JSON, invokes, and asserts the pool's PID changed — would fail if the kill+respawn were removed.

### BUG-02: Dead lambdas only discovered on next request
**File:** `src/process/mod.rs` — `spawn_process()`
**Problem:** No background liveness monitoring. If a Bun worker dies idle (OOM-kill, event-loop crash, etc.), the `ProcessHandle` stays in the pool as "warm." The first inbound request hits broken stdin → BrokenPipe → crash arm → respawn → that one caller gets a 502. Unnecessary failure.
**Fix:** Each `spawn_process` call spawns a `tokio::task` that `child.wait().await`s. On exit, triggers respawn and replaces the handle in the pool (must coordinate with the semaphore).

### BUG-03: Client disconnect desyncs the pipe
**File:** `src/process/mod.rs` — `invoke()` timeout wrapper (~line 128)
**Problem:** If axum drops the `invoke` future mid-flight (client disconnected during a long response), the write or read is abandoned. The lambda may finish writing its response to stdout, but the Rust side is gone. Next request reads stale bytes. Pipe desynced indefinitely.
**Fix:** Drop-guard or `tokio::select!` that kills+respawns the process if the future is cancelled before a clean read completes.

---

## P1 — Will OOM or break operations within days

### BUG-04: Unbounded log channel OOMs headless/prod deployments
**File:** `src/main.rs:114`, `src/server.rs` (push_log on every request)
**Problem:** `mpsc::unbounded_channel` is only drained by the TUI. With `--no-tui` (production mode), the receiver is never consumed. Every request pushes at least one `LogEntry`. At moderate load this accumulates gigabytes over hours.
**Fix:** Always spawn a drain task regardless of TUI mode. In headless mode: drain to `tracing::debug!` or just drop. Bound the channel (`mpsc::channel(10_000)`) so backpressure kicks in if the drain stalls.

### BUG-05: No graceful shutdown — children orphaned on SIGTERM
**File:** `src/main.rs:164`
**Problem:** `server::run(app_state, addr).await` returns only on error. SIGTERM kills the Rust process immediately; all child Bun processes are orphaned. Every in-flight request is dropped mid-flight.
**Fix:** Install SIGTERM/SIGINT handler via `tokio::signal`. On signal: stop accepting new connections, wait for in-flight requests to drain (timeout: 30s), then `kill_process_group` all pools, then exit. Wire into `axum::serve(...).with_graceful_shutdown(signal)`.

### BUG-06: Hot reload doesn't reload processes
**File:** `src/hotreload.rs`
**Problem:** Config file changes rebuild the `Router` and update `AppState.config` — but `process_manager` pools are never touched. Changing `handler`, `concurrency`, or adding a new route has no effect on running lambdas.
**Fix:** After rebuilding config, diff old vs new routes. For changed routes call `process_manager.hot_swap()`. For new routes call `spawn_all()`. For removed routes, drain and drop their pools.

### BUG-07: `/deploy` endpoint is fully open by default
**File:** `src/deploy.rs:59-68`, `src/main.rs:116-118`
**Problem:** With neither `deploy_key` nor `allowed_cidrs` set (the default), `POST /deploy` accepts unauthenticated arbitrary code execution from any IP. The startup warning is not sufficient.
**Fix:** In non-dev mode, if `effective_deploy_key().is_none() && allowed_cidrs.is_empty()`, disable `/deploy` or bind it to 127.0.0.1 only. Hard-fail or hard-warn at startup.

---

## P2 — Interop and correctness gaps

### BUG-08: URL path params not decoded
**File:** `src/router.rs` — path param extraction
**Problem:** `/accounts/foo%2Fbar` gives `id = "foo%2Fbar"` (raw percent-encoded). AWS API Gateway decodes path params before passing to lambdas. Handlers written for AWS get wrong values.
**Fix:** URL-decode each path param segment before inserting into `path_parameters`.

### BUG-09: Binary request/response bodies silently mangled
**File:** `src/server.rs:78` (request), `src/server.rs:162-176` (response)
**Problem:** `String::from_utf8_lossy` mangles binary uploads. Response side ignores `isBase64Encoded: true` from the lambda — passes raw base64 as the HTTP body.
**Fix:** Base64-encode non-text request bodies and set `is_base64_encoded: true`. On response: if `isBase64Encoded: true`, base64-decode before writing to the HTTP response.

### BUG-10: Oversized request body silently truncated to empty
**File:** `src/server.rs:72-74`
**Problem:** If body exceeds 10 MiB, `unwrap_or_default()` silently makes it empty. Lambda sees no body. Should be 413.
**Fix:** Match on the `Err` from `to_bytes` and return `StatusCode::PAYLOAD_TOO_LARGE`.

### BUG-11: No `/health` or `/ready` endpoint
**File:** `src/server.rs`
**Problem:** Any reverse proxy or load balancer needs a health endpoint. Absent.
**Fix:** `GET /health` → `{"status":"ok"}` always. `GET /ready` → 200 when all pools healthy, 503 with `{"unhealthy":["GET /foo"]}` otherwise.

### BUG-12: Cache ignores `Authorization`/`Cookie` headers
**File:** `src/server.rs:58-68`, `src/cache.rs`
**Problem:** Cache key is `method:path?query`. Two users with different tokens hit the same route → user B gets user A's cached response. Data leak for any authenticated endpoint.
**Fix:** Never cache when `Authorization` or `Cookie` is present, or include a hash of auth headers in the cache key.

### BUG-13: Deploy doesn't verify new processes survive startup
**File:** `src/deploy.rs` — after `hot_swap`
**Problem:** Response is `{"status":"ok"}` even if the new handler immediately crashes. Operator walks away while every request 502s.
**Fix:** After `hot_swap`, do a `try_wait()` + short delay health check. If the process is already dead, return 422 and revert.

---

## P3 — Performance and observability

### BUG-14: sysinfo scans all PIDs on every TUI tick
**File:** `src/process/mod.rs` — `pool_stats()` (~line 267)
**Problem:** `ProcessesToUpdate::All` walks every PID on the host every 100ms. Wasteful on loaded VPS.
**Fix:** Pass `ProcessesToUpdate::Some(&pids)` using the PIDs already collected in the async phase.

### BUG-15: `route_stats` write lock serializes all requests
**File:** `src/state.rs` — `record_request()`
**Problem:** Every request takes `route_stats.write().await` — a global write lock across all routes and all concurrency. Bottleneck at high throughput.
**Fix:** Per-route atomic counters + lock-free latency sampling (reservoir), or shard by route_key.

### BUG-16: Log lines missing request_id and source IP
**File:** `src/server.rs:136-141`
**Problem:** Access log format `"{method} {path} {status} {latency}ms"` has no `request_id`, no `source_ip`. Impossible to correlate failures to specific requests.
**Fix:** Include `request_id` (already generated at line 81) and `source_ip` in log format.

### BUG-17: Cache max_size_mb accounting is wrong
**File:** `src/cache.rs:36`
**Problem:** `max_capacity = max_size_mb * 1024 * 1024 / 512` assumes 512B per entry. A route returning 100 KB responses with `max_size_mb=128` stores 26 GB.
**Fix:** Use moka's weighted capacity (`weigher`) where each entry's weight is its actual byte size.

### BUG-18: Deploy staging dir races under concurrent deploys
**File:** `src/deploy.rs`
**Problem:** `remove_dir_all` then `create_dir_all` on the same `/tmp/riz-deploy/<lambda>` path races under concurrent deploys for the same lambda.
**Fix:** Write to a per-deploy `tempfile::TempDir` and atomic-rename into place.

### BUG-19: ZIP symlinks not rejected during deploy
**File:** `src/deploy.rs` — zip extraction loop
**Problem:** A zip entry that is a symlink (e.g. `./index.ts -> /etc/passwd`) gets extracted and Bun follows it. Path escape.
**Fix:** Reject any zip entry where `file.is_symlink()`.
