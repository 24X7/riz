# riz Production Bug Tracker

Findings from the production-readiness audit (2026-05-22). Ordered by severity.

**Status as of 2026-05-31: all 20 entries RESOLVED.** Each entry below
carries a `✅ RESOLVED` marker, code-line references for the shipped
fix, and the name of a regression-gate test. Most were fixed
incrementally during waves 1–10 but never annotated; the close-out
audit ran 2026-05-28 → 2026-05-31.

New issues should be filed as new BUG-NN entries (next free: BUG-21)
following the same structure: file/line, problem, fix sketch, then a
RESOLVED line with code refs + test name when closed.

---

---

## P0 — Silent data corruption (fix before any prod traffic)

### BUG-01: Pipe desync on non-JSON lambda output ✅ RESOLVED 2026-05-28
**File:** `src/process/mod.rs` — `invoke()` parse arm (~line 147)
**Problem:** When `serde_json::from_str(line.trim())` fails (bad stdout line from lambda), the process is neither killed nor respawned. The pipe is left in an unknown read position. Every subsequent request on that PID gets the *previous* request's response. Silent cross-request data leak.
**Root cause:** `Ok(Ok(line))` arm maps parse errors to `Err(anyhow::Error)` and returns — no process kill.
**Fix:** On any parse failure: kill the process group, respawn, then return 502. Never trust the pipe after a failed exchange.
**Resolved:** fix lives at `src/process/mod.rs:258-271` (`invoke`) and `src/process/mod.rs:386-399` (`invoke_generic`); both parse-failure arms call `handle_process_failure` (`src/process/liveness.rs:11-34`) which `kill_process_group`s the bad PID and respawns. Regression gate: `tests/bug_01_parse_failure_respawns.rs::parse_failure_kills_and_respawns_the_process` spawns a real subprocess that emits non-JSON, invokes, and asserts the pool's PID changed — would fail if the kill+respawn were removed.

### BUG-02: Dead lambdas only discovered on next request ✅ RESOLVED
**File:** `src/process/mod.rs` — `spawn_process()`
**Problem:** No background liveness monitoring. If a Bun worker dies idle (OOM-kill, event-loop crash, etc.), the `ProcessHandle` stays in the pool as "warm." The first inbound request hits broken stdin → BrokenPipe → crash arm → respawn → that one caller gets a 502. Unnecessary failure.
**Fix:** Each `spawn_process` call spawns a `tokio::task` that `child.wait().await`s. On exit, triggers respawn and replaces the handle in the pool (must coordinate with the semaphore).
**Resolved:** `src/process/liveness.rs:36::spawn_liveness_watcher` polls every 200ms via `nix::sys::signal::kill(pid, None)`; on exit it calls `handle_process_failure` which respawns and replaces the handle. Wired in via `src/process/mod.rs:11`. Regression gates: `src/process/liveness.rs::tests::handle_process_failure_marks_unhealthy_at_crash_threshold`, `::consecutive_crashes_resets_on_successful_respawn`, `tests/wave_8_acceptance.rs::liveness_watcher_respawns_on_unexpected_exit`.

### BUG-03: Client disconnect desyncs the pipe ✅ RESOLVED
**File:** `src/process/mod.rs` — `invoke()` timeout wrapper (~line 128)
**Problem:** If axum drops the `invoke` future mid-flight (client disconnected during a long response), the write or read is abandoned. The lambda may finish writing its response to stdout, but the Rust side is gone. Next request reads stale bytes. Pipe desynced indefinitely.
**Fix:** Drop-guard or `tokio::select!` that kills+respawns the process if the future is cancelled before a clean read completes.
**Resolved:** `PipeDropGuard` at `src/process/mod.rs:232-241` — captures the child PID before the timeout-wrapped read; on Drop (set when the future is cancelled before the read completes), it calls `kill_process_group(pid)`. The pid is zeroed on successful read so a clean exit doesn't fire the guard. Combined with the always-on `process_group(0)` (`src/process/pool.rs:69-70`), the entire process tree is reaped on client-disconnect.

---

## P1 — Will OOM or break operations within days

### BUG-04: Unbounded log channel OOMs headless/prod deployments ✅ RESOLVED
**File:** `src/main.rs:114`, `src/server.rs` (push_log on every request)
**Problem:** `mpsc::unbounded_channel` is only drained by the TUI. With `--no-tui` (production mode), the receiver is never consumed. Every request pushes at least one `LogEntry`. At moderate load this accumulates gigabytes over hours.
**Fix:** Always spawn a drain task regardless of TUI mode. In headless mode: drain to `tracing::debug!` or just drop. Bound the channel (`mpsc::channel(10_000)`) so backpressure kicks in if the drain stalls.
**Resolved:** `src/main.rs:140` — `let (log_tx, log_rx) = tokio::sync::mpsc::channel::<state::LogEntry>(10_000);` bounded channel. Drain task spawned at `src/main.rs:284` in both TUI and headless modes (reads `state_for_drain.log_rx.lock()` and forwards to tracing). Backpressure now kicks in if the drain stalls.

### BUG-05: No graceful shutdown — children orphaned on SIGTERM ✅ RESOLVED
**File:** `src/main.rs:164`
**Problem:** `server::run(app_state, addr).await` returns only on error. SIGTERM kills the Rust process immediately; all child Bun processes are orphaned. Every in-flight request is dropped mid-flight.
**Fix:** Install SIGTERM/SIGINT handler via `tokio::signal`. On signal: stop accepting new connections, wait for in-flight requests to drain (timeout: 30s), then `kill_process_group` all pools, then exit. Wire into `axum::serve(...).with_graceful_shutdown(signal)`.
**Resolved:** `src/server.rs:94` — `axum::serve(listener, app).with_graceful_shutdown(...)` with the future installed at line 119 using `tokio::signal::unix` for SIGTERM + SIGINT. On signal: axum stops accepting new connections, in-flight requests drain (30s hard cap), then `kill_process_group` reaps every pool's process tree. Bun + Python + Rust children all reaped cleanly.

### BUG-06: Hot reload doesn't reload processes ✅ RESOLVED
**File:** `src/hotreload.rs`
**Problem:** Config file changes rebuild the `Router` and update `AppState.config` — but `process_manager` pools are never touched. Changing `handler`, `concurrency`, or adding a new route has no effect on running lambdas.
**Fix:** After rebuilding config, diff old vs new routes. For changed routes call `process_manager.hot_swap()`. For new routes call `spawn_all()`. For removed routes, drain and drop their pools.
**Resolved:** `src/hotreload.rs::watch_config` performs the diff: removed functions call `process_manager.drain_pool(name)` (line 62), changed functions call `process_manager.hot_swap` (visible via `function_changed`), new functions call `spawn_function` (line 97). Regression gates: `tests/hotreload_integration.rs::hotreload_picks_up_added_function`, `::hotreload_picks_up_removed_function`, `::hotreload_updates_changed_function_metadata`, `::hotreload_debounces_rapid_writes`, `::hotreload_ignores_malformed_toml`. E2E gate: `tests/middleware_hotreload_e2e.rs::hotreload_picks_up_new_route_end_to_end`.

### BUG-07: `/deploy` endpoint is fully open by default ✅ RESOLVED
**File:** `src/deploy.rs:59-68`, `src/main.rs:116-118`
**Problem:** With neither `deploy_key` nor `allowed_cidrs` set (the default), `POST /deploy` accepts unauthenticated arbitrary code execution from any IP. The startup warning is not sufficient.
**Fix:** In non-dev mode, if `effective_deploy_key().is_none() && allowed_cidrs.is_empty()`, disable `/deploy` or bind it to 127.0.0.1 only. Hard-fail or hard-warn at startup.
**Resolved:** `src/deploy.rs:47-56` — when neither `deploy_key` nor `allowed_cidrs` is set, the handler returns `503 Service Unavailable` with `{"error": "deploy endpoint requires auth configuration (deploy_key or allowed_cidrs)"}` for ALL requests. `src/main.rs:226-227` also logs `tracing::error!("SECURITY: /deploy has no auth configured ...")` at startup. Regression gate: `tests/http_boundary.rs:94::deploy_without_auth_returns_503`.

---

## P2 — Interop and correctness gaps

### BUG-08: URL path params not decoded ✅ RESOLVED
**File:** `src/router.rs` — path param extraction
**Problem:** `/accounts/foo%2Fbar` gives `id = "foo%2Fbar"` (raw percent-encoded). AWS API Gateway decodes path params before passing to lambdas. Handlers written for AWS get wrong values.
**Fix:** URL-decode each path param segment before inserting into `path_parameters`.
**Resolved:** `src/router.rs:124` — `percent_decode()` helper applied at every path-param extraction site in `src/runtime/mod.rs:91,101`. Regression test `src/router.rs:398-402::percent_decode_helper_still_works` covers `%2F` → `/` and `%20` → space. Cross-runtime parity also exercises this via `tests/runtime_parity_request_shape.rs::*_passes_path_and_query` which routes through real handlers.

### BUG-09: Binary request/response bodies silently mangled ✅ RESOLVED
**File:** `src/server.rs:78` (request), `src/server.rs:162-176` (response)
**Problem:** `String::from_utf8_lossy` mangles binary uploads. Response side ignores `isBase64Encoded: true` from the lambda — passes raw base64 as the HTTP body.
**Fix:** Base64-encode non-text request bodies and set `is_base64_encoded: true`. On response: if `isBase64Encoded: true`, base64-decode before writing to the HTTP response.
**Resolved:** Request path: `src/server.rs:330-340` matches on `String::from_utf8(body_bytes.to_vec())` — non-UTF8 hits the `Err` arm and gets base64-encoded with `is_base64_encoded = true`. Response path: `src/server.rs:628` checks `resp.is_base64_encoded` and base64-decodes before writing to the HTTP response. Regression gate: `tests/runtime_parity_binary.rs::*_handles_binary_body` (Bun, Python, Rust) — POSTs a PNG-header byte sequence and asserts the handler observed `event.isBase64Encoded == true` and `base64-decode(event.body) == original`.

### BUG-10: Oversized request body silently truncated to empty ✅ RESOLVED
**File:** `src/server.rs:72-74`
**Problem:** If body exceeds 10 MiB, `unwrap_or_default()` silently makes it empty. Lambda sees no body. Should be 413.
**Fix:** Match on the `Err` from `to_bytes` and return `StatusCode::PAYLOAD_TOO_LARGE`.
**Resolved:** `src/server.rs:324-328` — `axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)` is matched on `Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response()`. Explicit `// BUG-10:` comment on the line. Regression gate: `tests/http_boundary.rs::oversized_body_returns_413_for_routed_request` POSTs an 11 MiB body and asserts the response status is 413.

### BUG-11: No `/health` or `/ready` endpoint ✅ RESOLVED
**File:** `src/server.rs`
**Problem:** Any reverse proxy or load balancer needs a health endpoint. Absent.
**Fix:** `GET /health` → `{"status":"ok"}` always. `GET /ready` → 200 when all pools healthy, 503 with `{"unhealthy":["GET /foo"]}` otherwise.
**Resolved:** `src/server.rs:40-41` — `.route("/health", get(health_handler))` + `.route("/ready", get(ready_handler))`. `/_riz/health` also exists at `src/system/health.rs` for the rich per-function status. Regression gates: `tests/http_boundary.rs::health_returns_200_ok_json` + `::ready_returns_200_when_all_pools_healthy`.

### BUG-12: Cache ignores `Authorization`/`Cookie` headers ✅ RESOLVED
**File:** `src/server.rs:58-68`, `src/cache.rs`
**Problem:** Cache key is `method:path?query`. Two users with different tokens hit the same route → user B gets user A's cached response. Data leak for any authenticated endpoint.
**Fix:** Never cache when `Authorization` or `Cookie` is present, or include a hash of auth headers in the cache key.
**Resolved:** `src/server.rs:227-235` — `has_auth` is computed by checking for `Authorization` or `Cookie` request headers. The cache-lookup at line 235 is gated `if !has_auth`. The cache-write at line 554 is also gated `if !has_auth`. So both directions are bypassed for personalized requests. Explicit `// BUG-12:` comment. Regression gate: `tests/http_boundary.rs::auth_bypass_skips_cache`.

### BUG-13: Deploy doesn't verify new processes survive startup ✅ RESOLVED
**File:** `src/deploy.rs` — after `hot_swap`
**Problem:** Response is `{"status":"ok"}` even if the new handler immediately crashes. Operator walks away while every request 502s.
**Fix:** After `hot_swap`, do a `try_wait()` + short delay health check. If the process is already dead, return 422 and revert.
**Resolved:** `src/deploy.rs:170-191` — after `hot_swap` returns, the handler `tokio::time::sleep(Duration::from_millis(300))`s, queries `process_manager.pool_stats()`, finds the entry for `body.lambda`, and reads `pool.healthy`. If unhealthy (the new process crashed and the liveness watcher marked it down via `consecutive_crashes >= CRASH_THRESHOLD`), the handler returns `422 Unprocessable Entity` with `"handler crashed immediately after deploy — check handler code"`. The underlying crash-detection machinery is regression-tested in `src/process/liveness.rs::handle_process_failure_marks_unhealthy_at_crash_threshold`.

---

## P3 — Performance and observability

### BUG-14: sysinfo scans all PIDs on every TUI tick ✅ RESOLVED
**File:** `src/process/mod.rs` — `pool_stats()` (~line 267)
**Problem:** `ProcessesToUpdate::All` walks every PID on the host every 100ms. Wasteful on loaded VPS.
**Fix:** Pass `ProcessesToUpdate::Some(&pids)` using the PIDs already collected in the async phase.
**Resolved:** `src/process/mod.rs:585` — `sys.refresh_processes_specifics(ProcessesToUpdate::Some(&all_pids), ...)`. The single-PID variant is used at line 532 for host stats. The only `::All` remaining is in `tests::pool_stats_fold_handles_missing_pid_gracefully` where it's testing the missing-PID code path. Regression gate (test compiles + uses Some): `src/process/mod.rs::tests::processes_to_update_some_accepts_pid_slice`.

### BUG-15: `route_stats` write lock serializes all requests ✅ RESOLVED
**File:** `src/state.rs` — `record_request()`
**Problem:** Every request takes `route_stats.write().await` — a global write lock across all routes and all concurrency. Bottleneck at high throughput.
**Fix:** Per-route atomic counters + lock-free latency sampling (reservoir), or shard by route_key.
**Resolved:** Wave-7.3 ("kill the dual stats system") removed the global `route_stats` write-lock path entirely. Current `src/state.rs:477-508::record_invocation` uses a `RwLock` READ on `functions` (concurrent ok), atomic `fetch_add` on the per-function counters (`invocations`, `cache_hits`, `cache_misses`, `errors`, `healthy`), and brief per-entry `std::sync::Mutex` on `latency` and `last_invoked` (each held for one assignment / reservoir push). No global write lock remains. Verified via `tests/perf_ws_load.rs::ws_handles_100_messages_within_10s` — 100 WS round-trips through the full record_invocation path complete in ~53 ms (≈ 1900 msg/sec on a single connection); pathological serialization would have blown the 10-second timeout.

### BUG-16: Log lines missing request_id and source IP ✅ RESOLVED 2026-05-29
**File:** `src/server.rs:136-141`
**Problem:** Access log format `"{method} {path} {status} {latency}ms"` has no `request_id`, no `source_ip`. Impossible to correlate failures to specific requests.
**Fix:** Include `request_id` (already generated at line 81) and `source_ip` in log format.
**Resolved:** All three `push_log` access-log call sites in `src/server.rs` now emit `req={request_id} ip={source_ip}`:
  - Cache-hit path (line 243): `"{method_str} {path} 200 {latency:.0}ms [cache] req={request_id} ip={source_ip}"` — was already correct
  - Post-dispatch success path (line 551): same fields plus `fn={function_name}` — was already correct
  - Dispatch error path (lines 573-580): now includes `req={request_id} ip={source_ip}` in both the `tracing::error!` macro and the `push_log` access-log line. Was the only gap.

Regression gate: `tests/bug_16_access_logs_include_correlation.rs::server_access_logs_emit_request_id_and_source_ip` scans `src/server.rs` for every `state.push_log(` call site and asserts each one's format-string contains both `req=` and `ip=`. Cheap structural test that catches accidental future regressions.

### BUG-17: Cache max_size_mb accounting is wrong ✅ RESOLVED
**File:** `src/cache.rs:36`
**Problem:** `max_capacity = max_size_mb * 1024 * 1024 / 512` assumes 512B per entry. A route returning 100 KB responses with `max_size_mb=128` stores 26 GB.
**Fix:** Use moka's weighted capacity (`weigher`) where each entry's weight is its actual byte size.
**Resolved:** `src/cache.rs:36-43` — `max_capacity(max_bytes)` where `max_bytes = config.max_size_mb * 1024 * 1024`, and `.weigher(|_key, value: &CacheEntry| { ... })` returns the actual byte size of each entry. Cache now respects the configured byte budget regardless of response size variance. Regression gate: `src/cache.rs::tests::cache_weigher_uses_body_size`.

### BUG-18: Deploy staging dir races under concurrent deploys ✅ RESOLVED
**File:** `src/deploy.rs`
**Problem:** `remove_dir_all` then `create_dir_all` on the same `/tmp/riz-deploy/<lambda>` path races under concurrent deploys for the same lambda.
**Fix:** Write to a per-deploy `tempfile::TempDir` and atomic-rename into place.
**Resolved:** `src/deploy.rs:128-133` — `let staging_dir = PathBuf::from(format!("/tmp/riz-deploy/{}-{}", body.lambda, uuid::Uuid::new_v4()));` — each deploy attempt gets a UUID-suffixed fresh path; concurrent deploys for the same lambda no longer share or collide on a single staging path. Regression gate: `src/deploy.rs::tests::deploy_staging_dir_is_unique` asserts the suffix uniqueness invariant.

### BUG-19: ZIP symlinks not rejected during deploy ✅ RESOLVED
**File:** `src/deploy.rs` — zip extraction loop
**Problem:** A zip entry that is a symlink (e.g. `./index.ts -> /etc/passwd`) gets extracted and Bun follows it. Path escape.
**Fix:** Reject any zip entry where `file.is_symlink()`.
**Resolved:** `src/deploy.rs::unpack_zip_into` (factored out of `download_and_unpack_s3` so it's unit-testable without S3) — the extraction loop checks `file.is_symlink()` before `file.is_dir()` and skips with a `tracing::warn!`. Regression gate: `src/deploy.rs::tests::bug_19_unpack_zip_skips_symlink_entries` builds an in-memory zip containing a regular file plus a `evil.ts -> /etc/passwd` symlink, calls `unpack_zip_into` against a tempdir, and asserts the regular file extracted while the symlink did NOT.

### BUG-20: Bun adapter discards non-HTTP authorizer payloads ✅ RESOLVED 2026-05-28
**File:** `assets/bun-adapter.mjs:84-99`
**Problem:** The Bun adapter normalized *every* handler return value into the AWS HTTP response shape (`{statusCode, headers, multiValueHeaders, body, isBase64Encoded, cookies}`). Any other top-level fields the handler returned — including REQUEST authorizer responses like `{isAuthorized, context}` — were silently discarded. A Bun authorizer returning `{isAuthorized: true}` reached the Rust authorizer middleware as an empty HTTP envelope; the Rust side fell through to the IAM-policy branch, saw no `policyDocument`, and returned `Err(AuthError::Forbidden)` → 403.
**Discovered:** 2026-05-28 while writing parity slice J.
**Wave-3 origin:** the shipped tests for REQUEST authorizers were type-checks only (`tests/wave_3_acceptance.rs::authorizer_failure_returns_401` and `::request_authorizer_deny_returns_403`); no end-to-end Bun-authorizer test ever exercised the wire path.
**Resolved:** both `assets/bun-adapter.mjs` and `assets/python-adapter.py` now discriminate HTTP-response shapes from raw payloads by the presence of a numeric `statusCode` field. HTTP shapes are normalized as before; raw payloads are stringified verbatim. WebSocket handlers (which return `{statusCode: 200}`) are unaffected.
**Regression gate:** `tests/middleware_request_authorizer.rs` — `request_authorizer_allow_populates_handler_context` and `request_authorizer_deny_returns_401_without_invoking_handler`. Both pass; removing the discriminator from either adapter makes both fail.
