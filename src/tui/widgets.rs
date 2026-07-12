use crate::state::{FunctionKind, LogEntry};
use crate::tui::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};
use std::collections::VecDeque;
use std::time::UNIX_EPOCH;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.area());
    // Layout::split yields one Rect per constraint; if that contract ever
    // breaks, skip the frame — a blank tick beats crashing the dev console.
    let (Some(&tab_bar), Some(&body)) = (chunks.first(), chunks.get(1)) else {
        return;
    };

    render_tabs(frame, app, tab_bar);

    match app.selected_tab {
        0 => render_routes(frame, app, body),
        1 => render_processes(frame, app, body),
        2 => render_cache(frame, app, body),
        3 => render_tokens(frame, app, body),
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
        .block(Block::default().borders(Borders::ALL).title("riz"))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn render_routes(frame: &mut Frame, app: &App, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    // One Rect per constraint (see render()); skip the frame if violated.
    let (Some(&table_area), Some(&log_area)) = (split.first(), split.get(1)) else {
        return;
    };

    render_routes_table(frame, app, table_area);
    render_log_panel(frame, app, log_area);
}

fn render_routes_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new([
        "", "Route", "Reqs", "Err", "Cold", "p50", "p75", "p90", "p95", "p99", "Hit%", "Health",
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = app
        .function_stats
        .iter()
        .enumerate()
        .map(|(i, stats)| {
            let cursor = if app.selected_route == Some(i) {
                "▶"
            } else {
                " "
            };
            let is_system = matches!(stats.kind, FunctionKind::System);
            let route_label = if is_system {
                format!("◆ {}", stats.name) // diamond marker for system routes
            } else {
                stats.name.clone()
            };
            let route_style = if is_system {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            let health_color = if stats.healthy {
                Color::Green
            } else {
                Color::Red
            };
            let cursor_style = if app.selected_route == Some(i) {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new([
                Cell::from(cursor).style(cursor_style),
                Cell::from(route_label).style(route_style),
                Cell::from(format!("{}", stats.invocations)),
                Cell::from(format!("{}", stats.errors)),
                Cell::from(format!("{}", stats.cold_starts)),
                Cell::from(format!("{:.1}", stats.p50_ms)),
                Cell::from(format!("{:.1}", stats.p75_ms)),
                Cell::from(format!("{:.1}", stats.p90_ms)),
                Cell::from(format!("{:.1}", stats.p95_ms)),
                Cell::from(format!("{:.1}", stats.p99_ms)),
                Cell::from(format!("{:.0}%", stats.hit_rate_pct())),
                Cell::from(if stats.healthy { "ok" } else { "down" })
                    .style(Style::default().fg(health_color)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),      // cursor
            Constraint::Percentage(26), // route
            Constraint::Length(6),      // reqs
            Constraint::Length(5),      // err
            Constraint::Length(5),      // cold
            Constraint::Length(7),      // p50
            Constraint::Length(7),      // p75
            Constraint::Length(7),      // p90
            Constraint::Length(7),      // p95
            Constraint::Length(7),      // p99
            Constraint::Length(6),      // hit%
            Constraint::Length(7),      // health
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Routes  [↑↓ / j k] ◆ = system"),
    );

    frame.render_widget(table, area);
}

fn render_log_panel(frame: &mut Frame, app: &App, area: Rect) {
    let selected_key: Option<&str> = app
        .selected_route
        .and_then(|i| app.function_stats.get(i))
        .map(|s| s.name.as_str());

    let title = match selected_key {
        Some(k) => format!("Logs — {k}  [Esc / c to clear filter]"),
        None => "Logs  (all routes)".into(),
    };

    let visible = filter_logs(&app.log_entries, selected_key);
    let max_lines = area.height.saturating_sub(2) as usize;
    // Tail window without slicing: `start <= len` by construction
    // (saturating_sub), and `skip` cannot panic even if that drifts.
    let start = visible.len().saturating_sub(max_lines);

    let lines: Vec<Line> = visible
        .iter()
        .skip(start)
        .map(|entry| {
            let ts = format_timestamp(entry);
            let color = match entry.level.as_str() {
                "ERROR" => Color::Red,
                "WARN" => Color::Yellow,
                _ => Color::White,
            };
            Line::from(vec![
                Span::styled(format!("{ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<5}  ", entry.level), Style::default().fg(color)),
                Span::raw(entry.message.clone()),
            ])
        })
        .collect();

    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

pub fn filter_logs<'a>(
    entries: &'a VecDeque<LogEntry>,
    route_key: Option<&str>,
) -> Vec<&'a LogEntry> {
    entries
        .iter()
        .filter(|e| match route_key {
            Some(k) => e.route_key.as_deref() == Some(k),
            None => true,
        })
        .collect()
}

fn format_timestamp(entry: &LogEntry) -> String {
    let secs = entry
        .timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

fn render_processes(frame: &mut Frame, app: &App, area: Rect) {
    // Three stacked sections: Host strip, Processes table (user + system), then padding.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(0)])
        .split(area);
    // One Rect per constraint (see render()); skip the frame if violated.
    let (Some(&host_area), Some(&table_area)) = (chunks.first(), chunks.get(1)) else {
        return;
    };
    render_host_strip(frame, app, host_area);
    render_processes_table(frame, app, table_area);
}

fn render_host_strip(frame: &mut Frame, app: &App, area: Rect) {
    let h = &app.host_stats;
    let mem_str = if h.memory_rss_mb < 1.0 {
        format!("{:.0} KB", h.memory_rss_mb * 1024.0)
    } else {
        format!("{:.1} MB", h.memory_rss_mb)
    };
    let uptime_str = format_uptime(app.uptime_secs);
    let line1 = Line::from(vec![
        Span::styled("PID: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            h.pid.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("Memory: ", Style::default().fg(Color::DarkGray)),
        Span::styled(mem_str, Style::default().fg(Color::White)),
        Span::raw("   "),
        Span::styled("CPU: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:.1}%", h.cpu_percent),
            Style::default().fg(Color::White),
        ),
        Span::raw("   "),
        Span::styled("Cores: ", Style::default().fg(Color::DarkGray)),
        Span::styled(h.cores.to_string(), Style::default().fg(Color::White)),
        Span::raw("   "),
        Span::styled("Uptime: ", Style::default().fg(Color::DarkGray)),
        Span::styled(uptime_str, Style::default().fg(Color::Green)),
    ]);
    let paragraph = Paragraph::new(line1).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Host — riz daemon"),
    );
    frame.render_widget(paragraph, area);
}

fn render_processes_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new([
        "", "Route", "PIDs", "Mem MB", "CPU%", "Conc", "Restarts", "Health",
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let mut rows: Vec<Row> = Vec::new();

    // System endpoints first — they share the riz host PID and contribute
    // to its memory/CPU. We render them with the host PID and N/A in the
    // restart column since they don't crash independently.
    for f in app
        .function_stats
        .iter()
        .filter(|f| matches!(f.kind, FunctionKind::System))
    {
        let health_color = if f.healthy { Color::Green } else { Color::Red };
        rows.push(Row::new([
            Cell::from("◆").style(Style::default().fg(Color::DarkGray)),
            Cell::from(f.name.clone()).style(Style::default().fg(Color::DarkGray)),
            Cell::from(app.host_stats.pid.to_string()).style(Style::default().fg(Color::DarkGray)),
            Cell::from("(host)").style(Style::default().fg(Color::DarkGray)),
            Cell::from("(host)").style(Style::default().fg(Color::DarkGray)),
            Cell::from("—").style(Style::default().fg(Color::DarkGray)),
            Cell::from("—").style(Style::default().fg(Color::DarkGray)),
            Cell::from(if f.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ]));
    }

    // User function pools.
    for s in &app.pool_stats {
        let pids: Vec<String> = s.pids.iter().map(|p| p.to_string()).collect();
        let health_color = if s.healthy { Color::Green } else { Color::Red };
        let mem_str = if s.memory_rss_mb < 1.0 {
            format!("{:.0}KB", s.memory_rss_mb * 1024.0)
        } else {
            format!("{:.1}", s.memory_rss_mb)
        };
        let cpu_str = format!("{:.1}%", s.cpu_percent);
        // Saturation: in-use / limit, with a ⚠ shed count when the pool has
        // load-shed. Colour rises with utilization so overload is visible at a
        // glance — red at the limit or after any shed, yellow past ~75%.
        let conc_str = if s.admission_rejected > 0 {
            format!(
                "{}/{} ⚠{}",
                s.concurrency_in_use, s.concurrency, s.admission_rejected
            )
        } else {
            format!("{}/{}", s.concurrency_in_use, s.concurrency)
        };
        let conc_color = if s.admission_rejected > 0
            || (s.concurrency > 0 && s.concurrency_in_use >= s.concurrency)
        {
            Color::Red
        } else if s.concurrency > 0
            && s.concurrency_in_use.saturating_mul(4) >= s.concurrency.saturating_mul(3)
        {
            Color::Yellow
        } else {
            Color::Reset
        };
        rows.push(Row::new([
            Cell::from(" "),
            Cell::from(s.name.as_str()),
            Cell::from(pids.join(", ")),
            Cell::from(mem_str),
            Cell::from(cpu_str),
            Cell::from(conc_str).style(Style::default().fg(conc_color)),
            Cell::from(s.restart_count.to_string()),
            Cell::from(if s.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Percentage(28),
            Constraint::Percentage(16),
            Constraint::Percentage(10),
            Constraint::Percentage(9),
            Constraint::Percentage(13),
            Constraint::Percentage(10),
            Constraint::Percentage(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Processes  ◆ = system (shares host)  ·  Conc = in-use/limit (⚠ shed)"),
    );

    frame.render_widget(table, area);
}

fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

fn render_cache(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!("Cached entries: {}", app.cache_entry_count);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Cache"))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_tokens(frame: &mut Frame, app: &App, area: Rect) {
    render_tokens_panel(frame, &app.token_stats, area);
}

/// Render the LLM token-utilization panel from a plain `TokenStatsSnapshot`.
/// Factored out of `render_tokens` so it can be exercised in isolation against
/// a ratatui `TestBackend` without constructing a full `App`.
///
/// Layout: a one-line cumulative summary (input / output / total) on top, then
/// a table of the most-recent chat-completions (`model · in→out`). LLM token
/// accounting is global/per-model (calls flow through the `/_riz/v1` gateway,
/// not per riz-function), so this is the system-wide view.
pub fn render_tokens_panel(
    frame: &mut Frame,
    stats: &crate::state::TokenStatsSnapshot,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);
    // One Rect per constraint (see render()); skip the frame if violated.
    let (Some(&summary_area), Some(&table_area)) = (chunks.first(), chunks.get(1)) else {
        return;
    };

    // ── Cumulative summary strip ──
    let summary = Line::from(vec![
        Span::styled("Input: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            stats.total_input.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("Output: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            stats.total_output.to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("Total: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            stats.total().to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let summary_panel = Paragraph::new(summary).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Tokens — cumulative"),
    );
    frame.render_widget(summary_panel, summary_area);

    // ── Recent chat-completions (newest at the bottom) ──
    let header = Row::new(["Model", "Provider", "In", "Out"]).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = stats
        .recent
        .iter()
        .map(|c| {
            Row::new([
                Cell::from(c.model.clone()),
                Cell::from(c.provider.clone()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(c.input.to_string()),
                Cell::from(format!("→{}", c.output)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(46),
            Constraint::Percentage(24),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Recent chat-completions  (model · in→out)"),
    );
    frame.render_widget(table, table_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LogEntry;
    use std::collections::VecDeque;
    use std::time::SystemTime;

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

    use crate::state::{TokenCall, TokenStatsSnapshot};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Flatten a rendered TestBackend buffer into a single string so we can
    /// assert on the displayed text regardless of cell layout.
    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn tokens_panel_renders_totals_and_recent_models() {
        let stats = TokenStatsSnapshot {
            total_input: 1234,
            total_output: 567,
            recent: vec![
                TokenCall {
                    model: "gpt-4o".into(),
                    provider: "openai".into(),
                    input: 100,
                    output: 40,
                    at: SystemTime::UNIX_EPOCH,
                },
                TokenCall {
                    model: "claude-sonnet".into(),
                    provider: "anthropic".into(),
                    input: 200,
                    output: 60,
                    at: SystemTime::UNIX_EPOCH,
                },
            ],
        };

        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_tokens_panel(f, &stats, f.area()))
            .unwrap();

        let text = buffer_text(&terminal);
        // Cumulative figures are displayed.
        assert!(text.contains("1234"), "input total missing: {text}");
        assert!(text.contains("567"), "output total missing");
        assert!(text.contains("1801"), "computed total (1234+567) missing");
        // Recent-call models + token figures are displayed.
        assert!(text.contains("gpt-4o"), "first model missing");
        assert!(text.contains("claude-sonnet"), "second model missing");
        assert!(text.contains("→40"), "first call output tokens missing");
    }

    #[test]
    fn tokens_panel_renders_empty_without_panic() {
        let stats = TokenStatsSnapshot::default();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_tokens_panel(f, &stats, f.area()))
            .unwrap();
        let text = buffer_text(&terminal);
        // Empty read-model still shows the zeroed cumulative strip.
        assert!(text.contains("Tokens"), "panel title missing: {text}");
    }

    /// Every tab must render on a degenerate (tiny) terminal without
    /// panicking — the saturating layout math and `.get()`-based chunk
    /// access degrade to clipped/blank panels instead of crashing the
    /// operator console. 3x2 leaves zero-height bodies after the tab bar.
    #[test]
    fn dashboard_renders_all_tabs_on_tiny_terminal_without_panic() {
        for tab in 0..App::tab_titles().len() {
            let mut app = App {
                selected_tab: tab,
                selected_route: Some(0),
                ..Default::default()
            };
            app.log_entries.push_back(make_entry(None, "one line"));
            for (w, h) in [(3u16, 2u16), (1, 1), (80, 3), (2, 24)] {
                let backend = TestBackend::new(w, h);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal
                    .draw(|f| render(f, &app))
                    .unwrap_or_else(|e| panic!("tab {tab} at {w}x{h} failed: {e}"));
            }
        }
    }

    /// The log panel shows the tail of the buffer: with more entries than
    /// visible lines, the oldest entries are skipped and the newest shown.
    #[test]
    fn log_panel_shows_most_recent_entries_when_overflowing() {
        let mut app = App {
            selected_tab: 0, // Routes tab hosts the log panel
            ..Default::default()
        };
        for i in 0..50 {
            app.log_entries
                .push_back(make_entry(None, &format!("msg-{i:02}")));
        }

        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("msg-49"), "newest entry missing: {text}");
        assert!(
            !text.contains("msg-00"),
            "oldest entry should be scrolled out of the tail window"
        );
    }

    #[test]
    fn dashboard_tokens_tab_renders_token_figures() {
        // Full dashboard render via the public `render(frame, app)` entrypoint,
        // proving the Tokens tab is wired end-to-end and doesn't panic.
        let app = App {
            selected_tab: 3, // Tokens
            token_stats: TokenStatsSnapshot {
                total_input: 42,
                total_output: 8,
                recent: vec![TokenCall {
                    model: "demo-model".into(),
                    provider: "mock".into(),
                    input: 7,
                    output: 3,
                    at: SystemTime::UNIX_EPOCH,
                }],
            },
            ..Default::default()
        };

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("demo-model"), "model not rendered: {text}");
        assert!(text.contains("42"), "input total not rendered");
        assert!(text.contains("Tokens"), "tab/title not rendered");
    }
}
