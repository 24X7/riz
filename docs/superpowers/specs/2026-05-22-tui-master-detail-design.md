# osbox — TUI Master-Detail + Request Logging Design

**Date:** 2026-05-22
**Status:** Approved

## Overview

Redesign the TUI Routes tab into a master-detail layout: selectable routes table on top, per-route log panel below. Log every HTTP request to the TUI (access log style). Capture lambda stderr in real time and surface it alongside request logs for the selected route.

**Goals:**
- Every HTTP request produces a visible log line in the TUI
- Lambda `console.log`/`console.error` output appears in the TUI alongside its route's traffic
- Operator can arrow-key through routes and see only that route's logs in the detail panel
- No tab-switching required to correlate traffic with logs

**Non-goals:**
- Log persistence to disk
- Log search or filtering beyond per-route selection
- Changing the lambda stdin/stdout protocol

---

## Data Model Changes

### `LogEntry` — add `route_key`

```rust
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub level: String,
    pub message: String,
    pub route_key: Option<String>,  // None = system/global (startup, hot-reload)
}
```

### `AppState` — replace `log_buffer` with a channel

`log_buffer: Mutex<VecDeque<LogEntry>>` is removed. In its place:

```rust
pub struct AppState {
    // ... existing fields ...
    pub log_tx: mpsc::UnboundedSender<LogEntry>,
    pub log_rx: Mutex<mpsc::UnboundedReceiver<LogEntry>>,
}
```

The channel is created in `main.rs` before `AppState` is constructed so the sender can also be passed to `ProcessManager::spawn_all` for stderr forwarding.

```rust
// main.rs, before Arc::new(AppState {...})
let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel::<LogEntry>();
process_manager.spawn_all(&config.routes, &registry, log_tx.clone()).await?;
let app_state = Arc::new(state::AppState {
    // ...
    log_tx,
    log_rx: tokio::sync::Mutex::new(log_rx),
});
```

### `push_log` — updated signature

```rust
impl AppState {
    pub fn push_log(&self, level: &str, route_key: Option<&str>, message: String) {
        let _ = self.log_tx.send(LogEntry {
            timestamp: SystemTime::now(),
            level: level.into(),
            message,
            route_key: route_key.map(|s| s.to_string()),
        });
    }
}
```

---

## What Gets Logged

### 1. Access log — every request

In `server.rs`, after the lambda responds (both cache hit and miss paths), push an INFO entry:

```rust
state.push_log(
    "INFO",
    Some(&route_key),
    format!("{method} {raw_path} {status} {latency:.0}ms"),
);
```

This replaces the current pattern of only logging on WARN/ERROR.

### 2. Lambda stderr — real-time background reader

`ProcessManager::spawn_all` gains a `log_tx: mpsc::UnboundedSender<LogEntry>` parameter. `spawn_process` receives a clone and spawns a background task:

```rust
if let Some(stderr) = child.stderr.take() {
    let tx = log_tx.clone();
    let key = route_key.to_string();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if line.is_empty() { continue; }
            let _ = tx.send(LogEntry {
                timestamp: SystemTime::now(),
                level: "WARN".into(),
                message: format!("stderr: {line}"),
                route_key: Some(key.clone()),
            });
        }
    });
}
```

This captures `console.log`, `console.error`, and any unhandled exception output from the TypeScript handler in real time, as long as the process is alive.

---

## TUI Changes

### `App` — route selection state

```rust
pub struct App {
    pub route_stats: Vec<(String, RouteStats)>,
    pub pool_stats: Vec<PoolStats>,
    pub cache_entry_count: u64,
    pub log_entries: VecDeque<LogEntry>,  // drained from log_rx each tick, capped at 500
    pub selected_tab: usize,
    pub selected_route: Option<usize>,    // index into route_stats; None = all logs shown
}

impl App {
    pub fn tab_titles() -> &'static [&'static str] {
        &["Routes", "Processes", "Cache"]  // "Logs" tab removed
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
```

### TUI run loop — drain channel each tick

In `tui/mod.rs`, each tick before drawing:

```rust
// drain log channel into app.log_entries
if let Ok(mut rx) = state.log_rx.try_lock() {
    while let Ok(entry) = rx.try_recv() {
        app.log_entries.push_back(entry);
        if app.log_entries.len() > 500 {
            app.log_entries.pop_front();
        }
    }
}
```

### Key bindings

```rust
KeyCode::Up | KeyCode::Char('k') => {
    if app.selected_tab == 0 { app.select_prev_route(); }
}
KeyCode::Down | KeyCode::Char('j') => {
    if app.selected_tab == 0 { app.select_next_route(); }
}
KeyCode::Tab | KeyCode::Right => app.next_tab(),
KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
KeyCode::Char('q') | KeyCode::Esc => break,
```

### Routes tab layout

Split vertically: 55% routes table, 45% log panel. The `▶` cursor marks the selected row. Log panel title and contents reflect the selection.

```
┌─ osbox ──────────────────────────────────────────────────┐
│  Routes   Processes   Cache                              │
├──────────────────────────────────────────────────────────┤
│  Route                  Reqs  p50ms  p95ms  Hit%  Hlth  │
│▶ GET /ping                12    2.1    4.3    0%    ok   │  55%
│  GET /accounts/:id         8    5.2    9.1   75%    ok   │
│  POST /events              3    3.8    7.2    0%    ok   │
├─ Logs — GET /ping ───────────────────────────────────────┤
│  12:34:01  INFO  GET /ping 200 2ms                       │  45%
│  12:34:03  INFO  GET /ping 200 1ms                       │
│  12:34:05  WARN  stderr: handling signup event           │
└──────────────────────────────────────────────────────────┘
```

**Log panel behavior:**
- When a route is selected: shows only entries where `log_entry.route_key == Some(selected_key)`
- When no route selected: shows all entries
- Newest entry at the bottom, auto-scrolls (no manual scroll)
- Log panel title: `"Logs — GET /ping"` (selected) or `"Logs"` (all)
- Log line format: `HH:MM:SS  LEVEL  message` (human-readable timestamp, not Unix epoch)

---

## Files Changed

| File | Change |
|------|--------|
| `src/state.rs` | `LogEntry` gains `route_key`; replace `log_buffer` with channel pair; update `push_log` signature |
| `src/main.rs` | Create channel before `AppState`; pass `log_tx.clone()` to `spawn_all` |
| `src/server.rs` | Push INFO access log on every request; add `route_key` to existing WARN/ERROR push calls |
| `src/process/mod.rs` | `spawn_all` gains `log_tx` param; `spawn_process` spawns background stderr reader task |
| `src/tui/app.rs` | Add `selected_route`; `select_next/prev_route`; remove "Logs" from tab titles |
| `src/tui/mod.rs` | Drain `log_rx` each tick; handle ↑/↓/j/k for route selection |
| `src/tui/widgets.rs` | Routes tab = vertical split master+detail; `▶` cursor; filtered log panel; remove `render_logs` |

---

## Testing

- Unit: `select_next_route` and `select_prev_route` boundary behavior (empty list, first/last)
- Unit: `push_log` sends entries with correct `route_key`
- Unit: log panel filter — entries with matching route_key shown, others hidden
- Existing 34 tests must continue to pass
