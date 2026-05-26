use crate::process::{HostStats, PoolStats};
use crate::state::{FunctionStateSnapshot, LogEntry};
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Default)]
pub struct App {
    /// Snapshot of all user-function state from RizState. Each tick the TUI
    /// rebuilds this list by calling FunctionState::snapshot() for every
    /// registered function (system endpoints are filtered out in tui::mod).
    pub function_stats: Vec<FunctionStateSnapshot>,
    pub pool_stats: Vec<PoolStats>,
    pub host_stats: HostStats,
    pub uptime_secs: u64,
    pub cache_entry_count: u64,
    pub log_entries: VecDeque<LogEntry>,
    pub selected_tab: usize,
    pub selected_route: Option<usize>,
    pub started_at: Option<Instant>,
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
        if self.function_stats.is_empty() {
            return;
        }
        self.selected_route = Some(match self.selected_route {
            None => 0,
            Some(i) => (i + 1).min(self.function_stats.len() - 1),
        });
    }

    pub fn select_prev_route(&mut self) {
        if self.function_stats.is_empty() {
            return;
        }
        self.selected_route = Some(match self.selected_route {
            None | Some(0) => 0,
            Some(i) => i - 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_routes(n: usize) -> App {
        let mut app = App::default();
        for i in 0..n {
            let s = FunctionStateSnapshot {
                name: format!("route{i}"),
                ..Default::default()
            };
            app.function_stats.push(s);
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
