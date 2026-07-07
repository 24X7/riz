use crate::process::{HostStats, PoolStats};
use crate::state::{FunctionStateSnapshot, LogEntry, TokenStatsSnapshot};
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
    /// Global LLM token utilization (cumulative + recent chat-completions).
    pub token_stats: TokenStatsSnapshot,
    pub selected_tab: usize,
    pub selected_route: Option<usize>,
    #[allow(dead_code)]
    pub started_at: Option<Instant>,
}

impl App {
    pub fn tab_titles() -> &'static [&'static str] {
        &["Routes", "Processes", "Cache", "Tokens"]
    }

    pub fn next_tab(&mut self) {
        // Wrap-around without `%`: checked_add + range filter keeps the
        // arithmetic panic-free (overflow-checks are on in release) while
        // behaving identically to `(tab + 1) % count` for in-range tabs.
        let count = Self::tab_titles().len();
        self.selected_tab = self
            .selected_tab
            .checked_add(1)
            .filter(|&t| t < count)
            .unwrap_or(0);
    }

    pub fn prev_tab(&mut self) {
        // checked_sub: 0 wraps to the last tab; saturating_sub keeps the
        // (non-empty by construction) titles length panic-free.
        self.selected_tab = self
            .selected_tab
            .checked_sub(1)
            .unwrap_or_else(|| Self::tab_titles().len().saturating_sub(1));
    }

    pub fn select_next_route(&mut self) {
        if self.function_stats.is_empty() {
            return;
        }
        self.selected_route = Some(match self.selected_route {
            None => 0,
            // saturating: clamp to the last index; the list is non-empty here.
            Some(i) => i
                .saturating_add(1)
                .min(self.function_stats.len().saturating_sub(1)),
        });
    }

    pub fn select_prev_route(&mut self) {
        if self.function_stats.is_empty() {
            return;
        }
        self.selected_route = Some(match self.selected_route {
            None | Some(0) => 0,
            Some(i) => i.saturating_sub(1),
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

    #[test]
    fn next_tab_cycles_through_all_and_wraps_to_zero() {
        let mut app = App::default();
        let count = App::tab_titles().len();
        for expected in 1..count {
            app.next_tab();
            assert_eq!(app.selected_tab, expected);
        }
        app.next_tab(); // from last tab
        assert_eq!(app.selected_tab, 0, "next from last tab wraps to first");
    }

    #[test]
    fn prev_tab_wraps_from_zero_to_last() {
        let mut app = App::default();
        assert_eq!(app.selected_tab, 0);
        app.prev_tab();
        assert_eq!(
            app.selected_tab,
            App::tab_titles().len() - 1,
            "prev from first tab wraps to last"
        );
        app.prev_tab();
        assert_eq!(app.selected_tab, App::tab_titles().len() - 2);
    }
}
