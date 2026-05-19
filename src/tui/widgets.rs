use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};
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
        3 => render_logs(frame, app, chunks[1]),
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
    let header = Row::new(["Route", "Req/s", "p50ms", "p95ms", "Hit%", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.route_stats.iter().map(|(key, stats)| {
        let rps = if stats.latencies_ms.is_empty() { 0.0 } else {
            1000.0 / stats.p50_ms().max(1.0)
        };
        let hit_pct = if stats.cache_hits + stats.cache_misses == 0 { 0.0 } else {
            stats.cache_hits as f64 / (stats.cache_hits + stats.cache_misses) as f64 * 100.0
        };
        let health_color = if stats.healthy { Color::Green } else { Color::Red };
        Row::new([
            Cell::from(key.as_str()),
            Cell::from(format!("{rps:.1}")),
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
            Constraint::Percentage(40),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Routes"));

    frame.render_widget(table, area);
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

fn render_logs(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app.log_entries.iter().rev().take(area.height.saturating_sub(2) as usize).map(|entry| {
        use std::time::UNIX_EPOCH;
        let secs = entry.timestamp.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let color = match entry.level.as_str() {
            "ERROR" => Color::Red,
            "WARN" => Color::Yellow,
            _ => Color::White,
        };
        Line::from(vec![
            Span::styled(format!("[{secs}] "), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[{}] ", entry.level), Style::default().fg(color)),
            Span::raw(entry.message.clone()),
        ])
    }).collect();

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Logs"));
    frame.render_widget(paragraph, area);
}
