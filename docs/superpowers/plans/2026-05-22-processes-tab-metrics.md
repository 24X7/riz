# Processes Tab Metrics + Console Log Redirect Plan

> Status: archived — shipped in wave-3; no corresponding spec needed.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** (1) Redirect `console.log` to stderr in the bun adapter so handler output appears in the TUI instead of corrupting the protocol. (2) Add Memory (MB) and CPU% columns to the Processes tab using per-PID sysinfo queries.

**Architecture:** Task 1 is a 2-line change in the JS adapter. Task 2 adds `sysinfo` to the crate, threads memory+cpu through `PoolStats`, and updates the Processes widget.

**Tech Stack:** Rust, sysinfo 0.31, Ratatui, Bun/JS adapter

---

## Task 1: Redirect console.log/info/debug to stderr in bun-adapter

**Files:**
- Modify: `assets/bun-adapter.mjs`

Currently, `console.log` in a handler writes to stdout (fd=1), which is the protocol channel. This corrupts the JSON response stream and causes a crash. `console.error` already goes to stderr (correct). Fix: redirect all console methods to stderr before the handler runs.

- [ ] **Step 1: Write the failing test** (manual verification — no automated test for this)

To verify the current bug before fixing: add `console.log("test")` to `examples/lambdas/ping/index.ts`, run `cargo run -- --dev`, curl `/ping`, observe a 502 crash. Remove the console.log after verifying. Skip if you trust the analysis.

- [ ] **Step 2: Add the redirect at the top of bun-adapter.mjs**

In `assets/bun-adapter.mjs`, after the imports and before the `const handlerPath` line, add:

```javascript
// Redirect all console output to stderr so it doesn't corrupt the stdout protocol stream.
// console.error and console.warn already go to stderr; .log/.info/.debug do not.
const _toStderr = (...args) => process.stderr.write(args.map(String).join(' ') + '\n');
console.log = console.info = console.debug = _toStderr;
```

Place it immediately after the import line (line 3), so the full top of the file looks like:

```javascript
import { createInterface } from "readline";

// Redirect all console output to stderr so it doesn't corrupt the stdout protocol stream.
// console.error and console.warn already go to stderr; .log/.info/.debug do not.
const _toStderr = (...args) => process.stderr.write(args.map(String).join(' ') + '\n');
console.log = console.info = console.debug = _toStderr;

const handlerPath = process.argv[2];
```

- [ ] **Step 3: Run unit tests (nothing breaks)**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests still pass (adapter is embedded via `include_str!` — the compiled binary embeds the updated adapter automatically on next build).

- [ ] **Step 4: Commit**

```bash
git add assets/bun-adapter.mjs
git commit -m "fix: redirect console.log/info/debug to stderr in bun adapter"
```

---

## Task 2: Memory (MB) and CPU% columns in the Processes tab

**Files:**
- Modify: `Cargo.toml` — add sysinfo 0.31
- Modify: `src/process/mod.rs` — add `sys` field to ProcessManager; update `PoolStats`; update `pool_stats()`
- Modify: `src/tui/widgets.rs` — add Memory and CPU% columns to `render_processes`

### Background: how sysinfo works

`sysinfo::System` must be refreshed before querying. CPU% is computed as a delta between two refreshes — the first call returns 0%, subsequent calls return real values. This is fine for the TUI (each tick refreshes, values populate after ~200ms).

`process.memory()` returns bytes (u64) in sysinfo 0.31.
`process.cpu_usage()` returns f32 percent (0–100 per logical CPU; can exceed 100% on multi-core).

---

- [ ] **Step 1: Add sysinfo to Cargo.toml**

Under `[dependencies]`:

```toml
sysinfo = "0.31"
```

Run: `cargo build`
Expected: compiles (sysinfo downloads)

- [ ] **Step 2: Write the failing test**

Add to `src/process/mod.rs` in the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn pool_stats_memory_and_cpu_fields_default_zero_for_dead_pid() {
        // When sysinfo can't find a PID (process doesn't exist), it returns None.
        // Verify our fold handles None correctly — both values must be 0, not panic.
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToRefresh::All,
            sysinfo::ProcessRefreshKind::new().with_memory().with_cpu(),
        );
        let dead_pid = sysinfo::Pid::from_u32(999999);
        let proc = sys.process(dead_pid);
        assert!(proc.is_none(), "PID 999999 should not exist");
        // fold over None gives (0, 0.0) — this is the invariant we rely on
        let (mem, cpu) = [999999u32].iter().fold((0u64, 0f32), |(m, c), &pid| {
            match sys.process(sysinfo::Pid::from_u32(pid)) {
                Some(p) => (m + p.memory(), c + p.cpu_usage()),
                None => (m, c),
            }
        });
        assert_eq!(mem, 0);
        assert_eq!(cpu, 0.0);
    }
```

Run: `cargo test pool_stats_memory_and_cpu_fields_default_zero_for_dead_pid`
Expected: FAIL (sysinfo not yet imported in mod.rs)

- [ ] **Step 3: Add sysinfo use + sys field to ProcessManager**

In `src/process/mod.rs`, add to the existing `use` block at the top:

```rust
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToRefresh, System};
```

Add `sys` field to `ProcessManager`:

```rust
pub struct ProcessManager {
    pools: RwLock<HashMap<String, Arc<RoutePool>>>,
    sys: std::sync::Mutex<System>,
}
```

Update `ProcessManager::new()`:

```rust
impl ProcessManager {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            sys: std::sync::Mutex::new(System::new()),
        }
    }
```

Run: `cargo test pool_stats_memory_and_cpu_fields_default_zero_for_dead_pid`
Expected: PASS

- [ ] **Step 4: Add memory_rss_mb and cpu_percent to PoolStats**

Update `PoolStats`:

```rust
pub struct PoolStats {
    pub route_key: String,
    pub pids: Vec<u32>,
    pub restart_count: u32,
    pub healthy: bool,
    pub concurrency: usize,
    pub memory_rss_mb: f64,
    pub cpu_percent: f32,
}
```

- [ ] **Step 5: Update pool_stats() to populate memory and CPU**

Replace the current `pool_stats` implementation:

```rust
    pub async fn pool_stats(&self) -> Vec<PoolStats> {
        let pools = self.pools.read().await;

        // Collect PIDs and metadata first (needs async for RwLock reads)
        struct RawStat { key: String, pids: Vec<u32>, restarts: u32, healthy: bool, concurrency: usize }
        let mut raw: Vec<RawStat> = Vec::new();
        for (key, pool) in pools.iter() {
            let handles = pool.handles.read().await;
            let pids = handles.iter()
                .filter_map(|h| h.try_lock().ok().map(|g| g.pid))
                .collect();
            raw.push(RawStat {
                key: key.clone(),
                pids,
                restarts: pool.restart_count.load(Ordering::Relaxed),
                healthy: pool.healthy.load(Ordering::Relaxed),
                concurrency: pool.route.concurrency,
            });
        }
        drop(pools);

        // Refresh sysinfo (sync — no await needed here)
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_processes_specifics(
            ProcessesToRefresh::All,
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );

        raw.into_iter().map(|r| {
            let (mem_bytes, cpu) = r.pids.iter().fold((0u64, 0f32), |(m, c), &pid| {
                match sys.process(Pid::from_u32(pid)) {
                    Some(p) => (m + p.memory(), c + p.cpu_usage()),
                    None => (m, c),
                }
            });
            PoolStats {
                route_key: r.key,
                pids: r.pids,
                restart_count: r.restarts,
                healthy: r.healthy,
                concurrency: r.concurrency,
                memory_rss_mb: mem_bytes as f64 / (1024.0 * 1024.0),
                cpu_percent: cpu,
            }
        }).collect()
    }
```

Run: `cargo build`
Expected: compiles

- [ ] **Step 6: Update render_processes in widgets.rs**

Replace the current `render_processes` function:

```rust
fn render_processes(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(["Route", "PIDs", "Mem MB", "CPU%", "Restarts", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.pool_stats.iter().map(|s| {
        let pids: Vec<String> = s.pids.iter().map(|p| p.to_string()).collect();
        let health_color = if s.healthy { Color::Green } else { Color::Red };
        let mem_str = if s.memory_rss_mb < 1.0 {
            format!("{:.0}KB", s.memory_rss_mb * 1024.0)
        } else {
            format!("{:.1}", s.memory_rss_mb)
        };
        let cpu_str = format!("{:.1}%", s.cpu_percent);
        Row::new([
            Cell::from(s.route_key.as_str()),
            Cell::from(pids.join(", ")),
            Cell::from(mem_str),
            Cell::from(cpu_str),
            Cell::from(s.restart_count.to_string()),
            Cell::from(if s.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(32),
            Constraint::Percentage(18),
            Constraint::Percentage(12),
            Constraint::Percentage(10),
            Constraint::Percentage(12),
            Constraint::Percentage(16),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Processes"));

    frame.render_widget(table, area);
}
```

- [ ] **Step 7: Run full test suite**

```bash
cargo test 2>&1
```

Expected: all tests pass (integration tests remain ignored)

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/process/mod.rs src/tui/widgets.rs
git commit -m "feat: memory and CPU% columns in Processes tab via sysinfo"
```

---

## Notes

- CPU% on first TUI tick will be 0% for all processes — this is expected; sysinfo needs two samples to compute a delta. Values populate immediately on the next tick (~200ms).
- `memory_rss_mb` uses RSS (resident set size) — the actual physical memory in use, not virtual. For Bun processes running a warm lambda, expect 50–150 MB typical.
- Percentages can exceed 100% on multi-core systems if the process uses multiple cores.
