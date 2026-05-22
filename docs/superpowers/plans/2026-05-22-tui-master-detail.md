# TUI Master-Detail + Request Logging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the Routes TUI tab into a selectable master-detail view with per-route logs, and log every HTTP request plus lambda stderr output in real time.

**Architecture:** `LogEntry` gains a `route_key` field. The `log_buffer` deque is replaced with an unbounded mpsc channel so the process module can forward lambda stderr without depending on `AppState`. The Routes TUI tab splits into a 55/45 vertical layout: selectable routes table on top, filtered log panel below.

**Tech Stack:** Rust, Ratatui, tokio::sync::mpsc (unbounded channel), crossterm.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/state.rs` | Modify | Add `route_key` to `LogEntry`; update `push_log` signature |
| `src/main.rs` | Modify | Wire channel; update `spawn_all` call; update `AppState` struct literal |
| `src/server.rs` | Modify | Push INFO access log on every request; update push_log callers |
| `src/process/mod.rs` | Modify | Real-time stderr reader per process; `log_tx` in `RoutePool` |
| `src/tui/app.rs` | Modify | Add `selected_route`; navigation methods; remove Logs tab |
| `src/tui/mod.rs` | Modify | Drain channel each tick; ↑/↓/j/k key bindings |
| `src/tui/widgets.rs` | Modify | Master-detail Routes tab; `▶` cursor; filtered log panel; remove render_logs |

---

## Task 1: Add `route_key` to `LogEntry` and update `push_log`

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server.rs` (fix callers to match new signature)

- [ ] **Step 1: Write a failing test for the new `LogEntry` field**

Add this test to the `#[cfg(test)] mod tests` block at the bottom of `src/state.rs` (after the existing `empty_returns_zero` test):

```rust
#[test]
fn log_entry_has_route_key_field() {
    let entry = LogEntry {
        timestamp: std::time::SystemTime::UNIX_EPOCH,
        level: "INFO".into(),
        message: "test".into(),
        route_key: Some("GET /ping".into()),
    };
    assert_eq!(entry.route_key.as_deref(), Some("GET /ping"));

    let global = LogEntry {
        timestamp: std::time::SystemTime::UNIX_EPOCH,
        level: "WARN".into(),
        message: "system".into(),
        route_key: None,
    };
    assert!(global.route_key.is_none());
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test log_entry_has_route_key_field 2>&1 | tail -5
```

Expected: compile error — `route_key` field does not exist on `LogEntry`.

- [ ] **Step 3: Add `route_key` to `LogEntry` in `src/state.rs`**

Replace the current `LogEntry` struct (lines 50-55):

```rust
#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub level: String,
    pub message: String,
    pub route_key: Option<String>,
}
```

- [ ] **Step 4: Update `push_log` to accept `route_key`**

Replace the `push_log` method body (currently lines 58-68):

```rust
pub async fn push_log(&self, level: &str, route_key: Option<&str>, message: String) {
    let mut buf = self.log_buffer.lock().await;
    if buf.len() >= 200 {
        buf.pop_front();
    }
    buf.push_back(LogEntry {
        timestamp: SystemTime::now(),
        level: level.into(),
        message,
        route_key: route_key.map(|s| s.to_string()),
    });
}
```

- [ ] **Step 5: Fix the two `push_log` callers in `src/server.rs`**

Line 133 — change:
```rust
state.push_log("WARN", format!("lambda {} returned {}", route_key, gw_resp.status_code)).await;
```
To:
```rust
state.push_log("WARN", Some(&route_key), format!("lambda {} returned {}", route_key, gw_resp.status_code)).await;
```

Line 148 — change:
```rust
state.push_log("ERROR", format!("dispatch error {route_key}: {e}")).await;
```
To:
```rust
state.push_log("ERROR", Some(&route_key), format!("dispatch error {route_key}: {e}")).await;
```

- [ ] **Step 6: Run all tests to confirm they pass**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected:
```
test result: ok. 30 passed; 0 failed; ...
test result: ok. 35 passed; 0 failed; ...
```

- [ ] **Step 7: Commit**

```bash
git add src/state.rs src/server.rs
git commit -m "feat: add route_key to LogEntry and push_log"
```

---

## Task 2: Replace `log_buffer` with unbounded channel

**Files:**
- Modify: `src/state.rs`
- Modify: `src/main.rs`
- Modify: `src/server.rs` (remove `.await` from push_log calls)
- Modify: `src/tui/mod.rs` (drain channel instead of cloning deque)

The goal: `push_log` becomes synchronous (a channel send), so the process module can call it without needing an async context or a full `Arc<AppState>`.

- [ ] **Step 1: Replace `log_buffer` with channel fields in `src/state.rs`**

Change the imports at the top of `src/state.rs` — add mpsc:

```rust
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{mpsc, Mutex, RwLock};
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::metrics::MetricsEmitter;
use crate::process::ProcessManager;
use crate::process::runtime::RuntimeRegistry;
use crate::router::Router;
```

Replace `log_buffer` in `AppState`:

```rust
pub struct AppState {
    pub config: RwLock<Config>,
    pub router: RwLock<Router>,
    pub process_manager: ProcessManager,
    pub cache: CacheLayer,
    pub metrics: MetricsEmitter,
    pub runtime_registry: Arc<RuntimeRegistry>,
    pub route_stats: RwLock<HashMap<String, RouteStats>>,
    pub log_tx: mpsc::UnboundedSender<LogEntry>,
    pub log_rx: Mutex<mpsc::UnboundedReceiver<LogEntry>>,
}
```

Replace `push_log` (make it synchronous — no `async`, no `.await`):

```rust
pub fn push_log(&self, level: &str, route_key: Option<&str>, message: String) {
    let _ = self.log_tx.send(LogEntry {
        timestamp: SystemTime::now(),
        level: level.into(),
        message,
        route_key: route_key.map(|s| s.to_string()),
    });
}
```

- [ ] **Step 2: Wire the channel in `src/main.rs`**

Create the channel before `spawn_all` and store it in AppState. **Do not pass `log_tx` to `spawn_all` yet — that signature change happens in Task 4.** Replace the current `ProcessManager::new()`, `spawn_all`, and `AppState` struct literal section:

```rust
let registry = Arc::new(process::runtime::RuntimeRegistry::new()?);
let cache = cache::CacheLayer::new(&config.cache);
let metrics = metrics::MetricsEmitter::new(&config.datadog);
let router = router::Router::new(config.routes.clone());
let process_manager = process::ProcessManager::new();

if config.effective_deploy_key().is_none() {
    tracing::warn!("SECURITY: no deploy key configured — POST /deploy is unauthenticated");
}

let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel::<state::LogEntry>();

process_manager.spawn_all(&config.routes, &registry).await?;

let app_state = Arc::new(state::AppState {
    config: tokio::sync::RwLock::new(config.clone()),
    router: tokio::sync::RwLock::new(router),
    process_manager,
    cache,
    metrics,
    runtime_registry: registry,
    route_stats: tokio::sync::RwLock::new(Default::default()),
    log_tx,
    log_rx: tokio::sync::Mutex::new(log_rx),
});
```

- [ ] **Step 3: Remove `.await` from `push_log` calls in `src/server.rs`**

Line 133 — remove `.await`:
```rust
state.push_log("WARN", Some(&route_key), format!("lambda {} returned {}", route_key, gw_resp.status_code));
```

Line 148 — remove `.await`:
```rust
state.push_log("ERROR", Some(&route_key), format!("dispatch error {route_key}: {e}"));
```

- [ ] **Step 4: Update `src/tui/mod.rs` to drain the channel instead of cloning the deque**

In `run_loop`, the current `block_on` block reads from `log_buffer`. Remove that part and add a channel drain outside the block_on. The full updated loop body:

```rust
fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: Arc<AppState>,
    handle: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    let mut app = App::default();
    let tick = Duration::from_millis(100);

    loop {
        handle.block_on(async {
            let route_stats = state.route_stats.read().await;
            app.route_stats = route_stats
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            app.pool_stats = state.process_manager.pool_stats().await;
            app.cache_entry_count = state.cache.entry_count();
        });

        // Drain log channel (synchronous — no block_on needed)
        if let Ok(mut rx) = state.log_rx.try_lock() {
            while let Ok(entry) = rx.try_recv() {
                app.log_entries.push_back(entry);
                if app.log_entries.len() > 500 {
                    app.log_entries.pop_front();
                }
            }
        }

        terminal.draw(|f| widgets::render(f, &app))?;

        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Tab | KeyCode::Right => app.next_tab(),
                    KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected: same pass counts as before, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/state.rs src/main.rs src/server.rs src/tui/mod.rs
git commit -m "refactor: replace log_buffer with unbounded channel for sync push_log"
```

---

## Task 3: Access log — push INFO on every request

**Files:**
- Modify: `src/server.rs`

Every HTTP request (cache hit or lambda invocation) should produce an INFO log entry visible in the TUI.

- [ ] **Step 1: Add INFO log to the cache-hit path in `src/server.rs`**

Find the cache-hit block (around lines 58-63):

```rust
if let Some(cached) = state.cache.get(&cache_key).await {
    let latency = start.elapsed().as_secs_f64() * 1000.0;
    state.record_request(&route_key, true, latency, true).await;
    state.metrics.record_cache_hit(&route_key);
    return gateway_to_axum(&cached);
}
```

Replace with:

```rust
if let Some(cached) = state.cache.get(&cache_key).await {
    let latency = start.elapsed().as_secs_f64() * 1000.0;
    state.record_request(&route_key, true, latency, true).await;
    state.metrics.record_cache_hit(&route_key);
    state.push_log(
        "INFO",
        Some(&route_key),
        format!("{method} {path} 200 {latency:.0}ms [cache]"),
    );
    return gateway_to_axum(&cached);
}
```

- [ ] **Step 2: Add INFO log to the lambda-success path**

In the `Ok(gw_resp)` match arm, after `state.record_request(...)` (around line 130), add:

```rust
state.push_log(
    "INFO",
    Some(&route_key),
    format!("{method} {path} {} {latency:.0}ms", gw_resp.status_code),
);
```

Place it just before the existing WARN push_log for 5xx responses. The relevant section becomes:

```rust
state.record_request(&route_key, false, latency, healthy).await;

state.push_log(
    "INFO",
    Some(&route_key),
    format!("{method} {path} {} {latency:.0}ms", gw_resp.status_code),
);

if gw_resp.status_code >= 500 {
    state.push_log("WARN", Some(&route_key), format!("lambda {} returned {}", route_key, gw_resp.status_code));
}
```

- [ ] **Step 3: Run all tests**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected: 0 failed.

- [ ] **Step 4: Smoke test — start osbox and confirm logs appear**

```bash
cargo run -- --dev &
sleep 2
curl -s http://localhost:3000/ping
# In the TUI, Routes tab should show a log entry for GET /ping 200 Xms
kill %1
```

- [ ] **Step 5: Commit**

```bash
git add src/server.rs
git commit -m "feat: push INFO access log entry on every request"
```

---

## Task 4: Real-time lambda stderr forwarding

**Files:**
- Modify: `src/process/mod.rs`
- Modify: `src/state.rs` (add import for mpsc in process module — actually state.rs already has it; process/mod.rs needs its own import of the channel type and LogEntry)

The current stderr reader uses `read_to_string` which blocks until the process dies. Replace it with a line-by-line reader that sends each line immediately to the log channel.

**Important:** `LogEntry` is defined in `src/state.rs`. `process/mod.rs` must import it. Since `state` is a sibling module and `process` is a submodule of `lib.rs`, use `crate::state::LogEntry`.

- [ ] **Step 1: Add `log_tx` to `RoutePool` in `src/process/mod.rs`**

Add `use tokio::sync::mpsc;` and `use crate::state::LogEntry;` to the imports at the top of `src/process/mod.rs`:

```rust
pub mod runtime;
pub mod bun;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};
use anyhow::Context;
use tracing::{error, warn};
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::runtime::RuntimeRegistry;
use crate::state::LogEntry;
```

Add `log_tx` field to `RoutePool`:

```rust
struct RoutePool {
    route: RouteConfig,
    handles: RwLock<Vec<Arc<Mutex<ProcessHandle>>>>,
    semaphore: Arc<Semaphore>,
    restart_count: AtomicU32,
    consecutive_crashes: AtomicU32,
    healthy: AtomicBool,
    runtime_registry: Arc<RuntimeRegistry>,
    log_tx: mpsc::UnboundedSender<LogEntry>,
}
```

- [ ] **Step 2: Update `spawn_all` to accept and store `log_tx`**

Change the signature and body of `spawn_all`:

```rust
pub async fn spawn_all(
    &self,
    routes: &[RouteConfig],
    registry: &Arc<RuntimeRegistry>,
    log_tx: mpsc::UnboundedSender<LogEntry>,
) -> anyhow::Result<()> {
    let mut pools = self.pools.write().await;
    for route in routes {
        let key = crate::router::Router::route_key(&route.method, &route.path);
        let pool = Arc::new(RoutePool {
            route: route.clone(),
            handles: RwLock::new(Vec::new()),
            semaphore: Arc::new(Semaphore::new(route.concurrency)),
            restart_count: AtomicU32::new(0),
            consecutive_crashes: AtomicU32::new(0),
            healthy: AtomicBool::new(true),
            runtime_registry: registry.clone(),
            log_tx: log_tx.clone(),
        });
        let mut handle_vec = pool.handles.write().await;
        for _ in 0..route.concurrency {
            let handle = spawn_process(route, registry, &log_tx).await
                .with_context(|| format!("failed to spawn lambda for {key}"))?;
            handle_vec.push(Arc::new(Mutex::new(handle)));
        }
        drop(handle_vec);
        pools.insert(key, pool);
    }
    Ok(())
}
```

- [ ] **Step 3: Update `spawn_process` to take `log_tx` and use a line-by-line stderr reader**

Replace the entire `spawn_process` function:

```rust
async fn spawn_process(
    route: &RouteConfig,
    registry: &RuntimeRegistry,
    log_tx: &mpsc::UnboundedSender<LogEntry>,
) -> anyhow::Result<ProcessHandle> {
    let runtime = registry.get(&route.runtime);
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .with_context(|| format!("failed to spawn {:?}", route.handler))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    if let Some(stderr) = child.stderr.take() {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        let tx = log_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() { continue; }
                let _ = tx.send(LogEntry {
                    timestamp: std::time::SystemTime::now(),
                    level: "WARN".into(),
                    message: format!("stderr: {line}"),
                    route_key: Some(route_key.clone()),
                });
            }
        });
    }

    Ok(ProcessHandle { pid, stdin, stdout, spawned_at: Instant::now(), _child: child })
}
```

- [ ] **Step 4: Update all internal `spawn_process` call sites to pass `&pool.log_tx`**

In `invoke`, the crash recovery (around line 147):
```rust
match spawn_process(&pool.route, &pool.runtime_registry, &pool.log_tx).await {
```

In `invoke`, the timeout recovery (around line 161):
```rust
match spawn_process(&pool.route, &pool.runtime_registry, &pool.log_tx).await {
```

In `hot_swap` (around line 198):
```rust
let h = spawn_process(&new_route, registry, &pool.log_tx).await?;
```

- [ ] **Step 5: Update `src/main.rs` to pass `log_tx` to `spawn_all`**

Now that `spawn_all` accepts `log_tx`, update the call site in `main.rs`. Change:

```rust
process_manager.spawn_all(&config.routes, &registry).await?;
```

To:

```rust
process_manager.spawn_all(&config.routes, &registry, log_tx.clone()).await?;
```

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected: 0 failed.

- [ ] **Step 7: Commit**

```bash
git add src/process/mod.rs src/main.rs
git commit -m "feat: real-time lambda stderr forwarding to TUI log channel"
```

---

## Task 5: Route selection in `App` and key bindings

**Files:**
- Modify: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Write failing tests for route selection navigation**

Add to the bottom of `src/tui/app.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RouteStats;

    fn app_with_routes(n: usize) -> App {
        let mut app = App::default();
        for i in 0..n {
            app.route_stats.push((format!("GET /route{i}"), RouteStats::default()));
        }
        app
    }

    #[test]
    fn select_next_from_none_goes_to_zero() {
        let mut app = app_with_routes(3);
        assert_eq!(app.selected_route, None);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn select_next_advances() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(0);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(1));
    }

    #[test]
    fn select_next_clamps_at_last() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(2);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(2));
    }

    #[test]
    fn select_prev_from_none_goes_to_zero() {
        let mut app = app_with_routes(3);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn select_prev_decrements() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(2);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(1));
    }

    #[test]
    fn select_prev_clamps_at_zero() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(0);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn no_routes_selection_is_noop() {
        let mut app = App::default();
        app.select_next_route();
        assert_eq!(app.selected_route, None);
        app.select_prev_route();
        assert_eq!(app.selected_route, None);
    }

    #[test]
    fn logs_tab_is_removed() {
        assert!(!App::tab_titles().contains(&"Logs"));
    }
}
```

- [ ] **Step 2: Run to confirm tests fail**

```bash
cargo test tui::app 2>&1 | tail -10
```

Expected: compile errors — `selected_route`, `select_next_route`, `select_prev_route` not defined.

- [ ] **Step 3: Implement selection in `src/tui/app.rs`**

Replace the entire file:

```rust
use std::collections::VecDeque;
use crate::state::{LogEntry, RouteStats};
use crate::process::PoolStats;

#[derive(Default)]
pub struct App {
    pub route_stats: Vec<(String, RouteStats)>,
    pub pool_stats: Vec<PoolStats>,
    pub cache_entry_count: u64,
    pub log_entries: VecDeque<LogEntry>,
    pub selected_tab: usize,
    pub selected_route: Option<usize>,
}

impl App {
    pub fn tab_titles() -> &'static [&'static str] {
        &["Routes", "Processes", "Cache"]
    }

    pub fn next_tab(&mut self) {
        self.selected_tab = (self.selected_tab + 1) % Self::tab_titles().len();
    }

    pub fn prev_tab(&mut self) {
        if self.selected_tab == 0 {
            self.selected_tab = Self::tab_titles().len() - 1;
        } else {
            self.selected_tab -= 1;
        }
    }

    pub fn select_next_route(&mut self) {
        if self.route_stats.is_empty() { return; }
        self.selected_route = Some(match self.selected_route {
            None => 0,
            Some(i) => (i + 1).min(self.route_stats.len() - 1),
        });
    }

    pub fn select_prev_route(&mut self) {
        if self.route_stats.is_empty() { return; }
        self.selected_route = Some(match self.selected_route {
            None | Some(0) => 0,
            Some(i) => i - 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RouteStats;

    fn app_with_routes(n: usize) -> App {
        let mut app = App::default();
        for i in 0..n {
            app.route_stats.push((format!("GET /route{i}"), RouteStats::default()));
        }
        app
    }

    #[test]
    fn select_next_from_none_goes_to_zero() {
        let mut app = app_with_routes(3);
        assert_eq!(app.selected_route, None);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn select_next_advances() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(0);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(1));
    }

    #[test]
    fn select_next_clamps_at_last() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(2);
        app.select_next_route();
        assert_eq!(app.selected_route, Some(2));
    }

    #[test]
    fn select_prev_from_none_goes_to_zero() {
        let mut app = app_with_routes(3);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn select_prev_decrements() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(2);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(1));
    }

    #[test]
    fn select_prev_clamps_at_zero() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(0);
        app.select_prev_route();
        assert_eq!(app.selected_route, Some(0));
    }

    #[test]
    fn no_routes_selection_is_noop() {
        let mut app = App::default();
        app.select_next_route();
        assert_eq!(app.selected_route, None);
        app.select_prev_route();
        assert_eq!(app.selected_route, None);
    }

    #[test]
    fn logs_tab_is_removed() {
        assert!(!App::tab_titles().contains(&"Logs"));
    }
}
```

- [ ] **Step 4: Add ↑/↓/j/k key bindings in `src/tui/mod.rs`**

In `run_loop`, replace the key event match inside `if event::poll(tick)?`:

```rust
if event::poll(tick)? {
    if let Event::Key(key) = event::read()? {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Tab | KeyCode::Right => app.next_tab(),
            KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
            KeyCode::Down | KeyCode::Char('j') => {
                if app.selected_tab == 0 { app.select_next_route(); }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if app.selected_tab == 0 { app.select_prev_route(); }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected: 8 new tests pass in `tui::app::tests`, 0 failed overall.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs src/tui/mod.rs
git commit -m "feat: route selection with j/k arrow keys in Routes TUI tab"
```

---

## Task 6: Master-detail widget

**Files:**
- Modify: `src/tui/widgets.rs`

Replace the Routes tab with a vertical split (55% table, 45% log panel). Add a `▶` cursor. Filter log entries by selected route. Remove the standalone `render_logs` function (the Logs tab no longer exists).

- [ ] **Step 1: Write a unit test for the log filter helper**

Add a `#[cfg(test)]` block at the bottom of `src/tui/widgets.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::SystemTime;
    use crate::state::LogEntry;

    fn make_entry(route_key: Option<&str>, msg: &str) -> LogEntry {
        LogEntry {
            timestamp: SystemTime::UNIX_EPOCH,
            level: "INFO".into(),
            message: msg.into(),
            route_key: route_key.map(|s| s.to_string()),
        }
    }

    #[test]
    fn filter_by_route_key_returns_matching_entries() {
        let mut entries = VecDeque::new();
        entries.push_back(make_entry(Some("GET /ping"), "ping 1"));
        entries.push_back(make_entry(Some("GET /accounts/:id"), "accounts 1"));
        entries.push_back(make_entry(Some("GET /ping"), "ping 2"));
        entries.push_back(make_entry(None, "system"));

        let visible = filter_logs(&entries, Some("GET /ping"));
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].message, "ping 1");
        assert_eq!(visible[1].message, "ping 2");
    }

    #[test]
    fn filter_with_none_returns_all() {
        let mut entries = VecDeque::new();
        entries.push_back(make_entry(Some("GET /ping"), "a"));
        entries.push_back(make_entry(None, "b"));

        let visible = filter_logs(&entries, None);
        assert_eq!(visible.len(), 2);
    }
}
```

- [ ] **Step 2: Run to confirm the test fails**

```bash
cargo test tui::widgets 2>&1 | tail -5
```

Expected: compile error — `filter_logs` function does not exist.

- [ ] **Step 3: Replace the entire `src/tui/widgets.rs`**

```rust
use std::collections::VecDeque;
use std::time::UNIX_EPOCH;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};
use crate::state::LogEntry;
use crate::tui::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.area());

    render_tabs(frame, app, chunks[0]);

    match app.selected_tab {
        0 => render_routes(frame, app, chunks[1]),
        1 => render_processes(frame, app, chunks[1]),
        2 => render_cache(frame, app, chunks[1]),
        _ => {}
    }
}

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = App::tab_titles()
        .iter()
        .map(|t| Line::from(Span::raw(*t)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.selected_tab)
        .block(Block::default().borders(Borders::ALL).title("osbox"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn render_routes(frame: &mut Frame, app: &App, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    render_routes_table(frame, app, split[0]);
    render_log_panel(frame, app, split[1]);
}

fn render_routes_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(["", "Route", "Reqs", "p50ms", "p95ms", "Hit%", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.route_stats.iter().enumerate().map(|(i, (key, stats))| {
        let cursor = if app.selected_route == Some(i) { "▶" } else { " " };
        let rps = if stats.latencies_ms.is_empty() { 0.0 } else {
            1000.0 / stats.p50_ms().max(1.0)
        };
        let hit_pct = if stats.cache_hits + stats.cache_misses == 0 { 0.0 } else {
            stats.cache_hits as f64 / (stats.cache_hits + stats.cache_misses) as f64 * 100.0
        };
        let health_color = if stats.healthy { Color::Green } else { Color::Red };
        let cursor_style = if app.selected_route == Some(i) {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Row::new([
            Cell::from(cursor).style(cursor_style),
            Cell::from(key.as_str()),
            Cell::from(format!("{}", stats.request_count)),
            Cell::from(format!("{:.1}", stats.p50_ms())),
            Cell::from(format!("{:.1}", stats.p95_ms())),
            Cell::from(format!("{hit_pct:.0}%")),
            Cell::from(if stats.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Percentage(38),
            Constraint::Percentage(10),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(14),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Routes  [↑↓ / j k to select]"));

    frame.render_widget(table, area);
}

fn render_log_panel(frame: &mut Frame, app: &App, area: Rect) {
    let selected_key: Option<&str> = app.selected_route
        .and_then(|i| app.route_stats.get(i))
        .map(|(k, _)| k.as_str());

    let title = match selected_key {
        Some(k) => format!("Logs — {k}"),
        None => "Logs".into(),
    };

    let visible = filter_logs(&app.log_entries, selected_key);
    let max_lines = area.height.saturating_sub(2) as usize;
    let start = visible.len().saturating_sub(max_lines);

    let lines: Vec<Line> = visible[start..].iter().map(|entry| {
        let ts = format_timestamp(entry);
        let color = match entry.level.as_str() {
            "ERROR" => Color::Red,
            "WARN" => Color::Yellow,
            _ => Color::White,
        };
        Line::from(vec![
            Span::styled(format!("{ts}  "), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:<5}  ", entry.level),
                Style::default().fg(color),
            ),
            Span::raw(entry.message.clone()),
        ])
    }).collect();

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

pub fn filter_logs<'a>(entries: &'a VecDeque<LogEntry>, route_key: Option<&str>) -> Vec<&'a LogEntry> {
    entries.iter().filter(|e| {
        match route_key {
            Some(k) => e.route_key.as_deref() == Some(k),
            None => true,
        }
    }).collect()
}

fn format_timestamp(entry: &LogEntry) -> String {
    let secs = entry.timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

fn render_processes(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(["Route", "PIDs", "Restarts", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.pool_stats.iter().map(|s| {
        let pids: Vec<String> = s.pids.iter().map(|p| p.to_string()).collect();
        let health_color = if s.healthy { Color::Green } else { Color::Red };
        Row::new([
            Cell::from(s.route_key.as_str()),
            Cell::from(pids.join(", ")),
            Cell::from(s.restart_count.to_string()),
            Cell::from(if s.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Processes"));

    frame.render_widget(table, area);
}

fn render_cache(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!("Cached entries: {}", app.cache_entry_count);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Cache"))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::SystemTime;
    use crate::state::LogEntry;

    fn make_entry(route_key: Option<&str>, msg: &str) -> LogEntry {
        LogEntry {
            timestamp: SystemTime::UNIX_EPOCH,
            level: "INFO".into(),
            message: msg.into(),
            route_key: route_key.map(|s| s.to_string()),
        }
    }

    #[test]
    fn filter_by_route_key_returns_matching_entries() {
        let mut entries = VecDeque::new();
        entries.push_back(make_entry(Some("GET /ping"), "ping 1"));
        entries.push_back(make_entry(Some("GET /accounts/:id"), "accounts 1"));
        entries.push_back(make_entry(Some("GET /ping"), "ping 2"));
        entries.push_back(make_entry(None, "system"));

        let visible = filter_logs(&entries, Some("GET /ping"));
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].message, "ping 1");
        assert_eq!(visible[1].message, "ping 2");
    }

    #[test]
    fn filter_with_none_returns_all() {
        let mut entries = VecDeque::new();
        entries.push_back(make_entry(Some("GET /ping"), "a"));
        entries.push_back(make_entry(None, "b"));

        let visible = filter_logs(&entries, None);
        assert_eq!(visible.len(), 2);
    }
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test 2>&1 | grep -E "^(test result|FAILED)"
```

Expected: 2 new tests pass in `tui::widgets::tests`, 0 failed overall.

- [ ] **Step 5: Smoke test the full TUI**

```bash
cargo run -- --dev &
sleep 2
# Hit all three routes a few times
curl -s http://localhost:3000/ping
curl -s "http://localhost:3000/accounts/42?include=profile"
curl -s -X POST http://localhost:3000/events \
  -H "content-type: application/json" \
  -d '{"type":"test"}'
# In the TUI: Routes tab should show the table with ▶ cursor on first route
# Press j/k to move the cursor — log panel should update to show only that route's logs
# Press Tab to switch to Processes, then back
kill %1
```

- [ ] **Step 6: Commit**

```bash
git add src/tui/widgets.rs
git commit -m "feat: master-detail Routes TUI with per-route log panel"
```

---

## Self-Review Checklist

| Spec requirement | Task |
|-----------------|------|
| `LogEntry.route_key: Option<String>` | Task 1 |
| `push_log` takes `route_key: Option<&str>` | Task 1 |
| Replace `log_buffer` with channel pair | Task 2 |
| `push_log` synchronous | Task 2 |
| Channel wired in `main.rs` before AppState | Task 2 |
| TUI drains channel each tick | Task 2 |
| INFO access log on every request (cache hit + lambda) | Task 3 |
| Real-time stderr forwarding via BufReader::lines | Task 4 |
| `log_tx` stored in `RoutePool` for respawn/hot_swap | Task 4 |
| `App.selected_route: Option<usize>` | Task 5 |
| `select_next/prev_route` with boundary clamping | Task 5 |
| "Logs" tab removed from tab_titles | Task 5 |
| ↑/↓/j/k bindings active only on Routes tab | Task 5 |
| Routes tab: 55% table / 45% log panel | Task 6 |
| `▶` cursor on selected row | Task 6 |
| Log panel filters to selected route | Task 6 |
| HH:MM:SS timestamp format | Task 6 |
| Log panel shows all when no route selected | Task 6 |
| All existing tests continue to pass | Every task |
