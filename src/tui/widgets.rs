use std::collections::VecDeque;
use std::time::UNIX_EPOCH;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};
use crate::state::{FunctionKind, LogEntry};
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
        .block(Block::default().borders(Borders::ALL).title("riz"))
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
    let header = Row::new([
        "", "Route", "Reqs", "Err", "Cold",
        "p50", "p75", "p90", "p95", "p99",
        "Hit%", "Health",
    ])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.function_stats.iter().enumerate().map(|(i, stats)| {
        let cursor = if app.selected_route == Some(i) { "▶" } else { " " };
        let is_system = matches!(stats.kind, FunctionKind::System);
        let route_label = if is_system {
            format!("◆ {}", stats.route_key)  // diamond marker for system routes
        } else {
            stats.route_key.clone()
        };
        let route_style = if is_system {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        let health_color = if stats.healthy { Color::Green } else { Color::Red };
        let cursor_style = if app.selected_route == Some(i) {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
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
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),       // cursor
            Constraint::Percentage(26),  // route
            Constraint::Length(6),       // reqs
            Constraint::Length(5),       // err
            Constraint::Length(5),       // cold
            Constraint::Length(7),       // p50
            Constraint::Length(7),       // p75
            Constraint::Length(7),       // p90
            Constraint::Length(7),       // p95
            Constraint::Length(7),       // p99
            Constraint::Length(6),       // hit%
            Constraint::Length(7),       // health
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Routes  [↑↓ / j k] ◆ = system"));

    frame.render_widget(table, area);
}

fn render_log_panel(frame: &mut Frame, app: &App, area: Rect) {
    let selected_key: Option<&str> = app.selected_route
        .and_then(|i| app.function_stats.get(i))
        .map(|s| s.route_key.as_str());

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
