# Process Isolation Implementation Plan

> Status: archived — shipped in wave-3; no corresponding spec needed.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two targeted hardening changes: (1) return 429 when all concurrency slots are in use instead of blocking indefinitely, and (2) kill the entire process group on timeout/crash so child processes spawned by the lambda don't survive.

**Architecture:** Both changes live entirely in `src/process/mod.rs`. Task 1 is a one-line fix. Task 2 adds a `nix` dependency and a small helper function, then calls it in two existing kill sites.

**Tech Stack:** Rust, tokio, nix 0.29 (new dep for killpg)

---

## What Already Exists (No Work Needed)

- `RoutePool.semaphore: Arc<Semaphore>` — concurrency cap structure already in place
- `route.concurrency` config field — already in `RouteConfig` and TOML files (default: 1)
- Zombie reaping — tokio's `Child` drop registers SIGCHLD handler automatically; no accumulation issue
- RLIMIT_AS — skip (breaks Bun/Python JIT; not worth it for self-hosted tool)
- CPU cgroups — skip (requires root, overkill for v0.1)

---

## Task 1: Return 429 when all concurrency slots are busy

**Files:**
- Modify: `src/process/mod.rs`

The current `invoke` function at line 106 does `pool.semaphore.acquire().await?` which blocks until a slot opens. Under a traffic spike this piles up goroutines waiting instead of failing fast. Change to `try_acquire()` which returns immediately with an error when all permits are taken.

- [ ] **Step 1: Write the failing test**

Add to `src/process/mod.rs` at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn semaphore_exhausted_returns_error_not_block() {
        let sem = tokio::sync::Semaphore::new(2);
        let _p1 = sem.try_acquire().expect("first permit");
        let _p2 = sem.try_acquire().expect("second permit");
        // All permits taken — try_acquire must fail immediately, not block
        assert!(
            sem.try_acquire().is_err(),
            "expected TryAcquireError when semaphore exhausted"
        );
    }
}
```

Run: `cargo test semaphore_exhausted_returns_error_not_block`
Expected: PASS (this tests the tokio API we're about to rely on — establishes the contract)

- [ ] **Step 2: Change acquire().await to try_acquire() in invoke**

In `src/process/mod.rs`, find this block (around line 105-107):

```rust
        // Acquire permit: guarantees at least one handle is free
        let _permit = pool.semaphore.acquire().await?;
```

Replace with:

```rust
        // Fail fast when all slots are busy — don't queue indefinitely
        let _permit = match pool.semaphore.try_acquire() {
            Ok(p) => p,
            Err(_) => return Ok(GatewayResponse::error(429, "too many concurrent requests")),
        };
```

- [ ] **Step 3: Run full test suite**

```bash
cargo test
```

Expected: All tests pass (the 429 path is new; existing tests don't hit the semaphore limit)

- [ ] **Step 4: Commit**

```bash
git add src/process/mod.rs
git commit -m "fix: return 429 immediately when concurrency cap is reached"
```

---

## Task 2: Kill entire process group on timeout and crash

**Files:**
- Modify: `Cargo.toml` — add nix
- Modify: `src/process/mod.rs` — set process_group(0) at spawn; add kill_process_group helper; call it in two kill sites

**Why this matters:** Bun can spawn subprocesses (e.g., for worker threads or child_process calls). When a lambda times out or crashes and we only kill the parent, those children keep running silently and accumulate. `killpg` kills the entire process tree rooted at the lambda.

**How it works:**
1. At spawn: `cmd.process_group(0)` tells the OS to create a new process group with the child's own PID as the PGID.
2. At kill: `killpg(pgid=child.pid, SIGKILL)` kills every process in that group — parent and all descendants.

- [ ] **Step 1: Add nix dependency**

In `Cargo.toml`, add to `[dependencies]`:

```toml
nix = { version = "0.29", features = ["process", "signal"] }
```

Run: `cargo build`
Expected: Compiles (nix 0.29 is stable; no API changes needed)

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)]` block in `src/process/mod.rs` (alongside the test from Task 1):

```rust
    #[test]
    fn kill_process_group_nonexistent_pid_does_not_panic() {
        // PID 99999 almost certainly does not exist.
        // killpg with a dead pgid returns ESRCH which we silently discard.
        // This test ensures the helper doesn't panic on the error path.
        kill_process_group(99999);
    }
```

Run: `cargo test kill_process_group_nonexistent_pid_does_not_panic`
Expected: FAIL (function doesn't exist yet)

- [ ] **Step 3: Add kill_process_group helper**

Add these two functions to `src/process/mod.rs`, just before the `spawn_process` function:

```rust
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}
```

No top-level `use nix::...` import is needed — the helper uses fully-qualified paths (`nix::sys::signal::killpg`, `nix::unistd::Pid::from_raw`) so it's self-contained.

Run: `cargo test kill_process_group_nonexistent_pid_does_not_panic`
Expected: PASS

- [ ] **Step 4: Set process group at spawn**

In `spawn_process`, find where the `cmd` is built (around line 239-242):

```rust
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());
```

Add the process_group call after the stdio setup:

```rust
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
```

`process_group(0)` means "create a new process group; use child's own PID as the PGID." After this, `handle.pid` is the PGID for the entire process tree.

Run: `cargo build`
Expected: Compiles cleanly

- [ ] **Step 5: Use killpg in the timeout arm**

In `invoke`, find the `Err(_)` (timeout) arm around line 162-175:

```rust
            Err(_) => {
                warn!("lambda timeout on {route_key} after {timeout_ms}ms — killing and restarting");
                let _ = handle._child.kill().await;
```

Change to:

```rust
            Err(_) => {
                warn!("lambda timeout on {route_key} after {timeout_ms}ms — killing and restarting");
                kill_process_group(handle.pid);
                let _ = handle._child.kill().await;
```

`kill_process_group` runs first (synchronous SIGKILL to the whole group), then we also call `_child.kill()` to ensure tokio marks the child as killed and triggers its SIGCHLD handler for reaping.

- [ ] **Step 6: Use killpg in the crash arm**

In `invoke`, find the `Ok(Err(e))` (crash) arm around line 142-158:

```rust
            Ok(Err(e)) => {
                pool.restart_count.fetch_add(1, Ordering::Relaxed);
                let crashes = pool.consecutive_crashes.fetch_add(1, Ordering::Relaxed) + 1;
                if crashes >= CRASH_THRESHOLD {
                    pool.healthy.store(false, Ordering::Relaxed);
                    error!("route {route_key} marked unhealthy after {crashes} crashes");
                }
                warn!("lambda crash on {route_key}: {e} — restarting");
                let _ = handle._child.kill().await;
```

Change the `handle._child.kill()` line to:

```rust
                warn!("lambda crash on {route_key}: {e} — restarting");
                kill_process_group(handle.pid);
                let _ = handle._child.kill().await;
```

The process already crashed (hence the I/O error), so `killpg` will get ESRCH for the main process but still catches any surviving children. The `let _ =` discard in the helper absorbs the error.

- [ ] **Step 7: Run full test suite**

```bash
cargo test
```

Expected: All tests pass (kill_process_group is a fire-and-forget no-op in tests; process_group(0) only affects spawned children which aren't spawned in unit tests)

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/process/mod.rs
git commit -m "feat: kill entire process group on lambda timeout/crash"
```

---

## Testing Notes

**What these tests cover:**
- `semaphore_exhausted_returns_error_not_block` — verifies the TryAcquireError contract we rely on
- `kill_process_group_nonexistent_pid_does_not_panic` — verifies the helper handles the dead-process error path without panicking

**What's not unit-testable:**
- The 429 path end-to-end (requires spawning a real bun process and saturating its permits)
- The killpg correctness (requires spawning a process that itself spawns children, then verifying they're all dead)

Both are verifiable manually: `cargo run -- --dev` then hammer a route with `ab -n 100 -c 50 http://localhost:3000/ping` — you should see 429s once the concurrency cap is hit. For killpg: add a `setInterval(() => {}, 1000)` to a lambda to keep it alive, trigger a timeout, verify no lingering bun processes with `ps aux | grep bun`.
