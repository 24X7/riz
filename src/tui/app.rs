use crate::process::{HostStats, PoolStats};
use crate::state::{FunctionStateSnapshot, LogEntry, TokenStatsSnapshot};
use crossterm::event::KeyCode;
use std::collections::VecDeque;
use std::time::Instant;

/// Log-panel severity filter, cycled with `l`. Exact-match (not threshold) so
/// "show me only warnings" is unambiguous.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevelFilter {
    #[default]
    All,
    Info,
    Warn,
    Error,
}

impl LogLevelFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Info,
            Self::Info => Self::Warn,
            Self::Warn => Self::Error,
            Self::Error => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// True if a log entry at `level` passes this filter.
    pub fn matches(self, level: &str) -> bool {
        match self {
            Self::All => true,
            Self::Info => level.eq_ignore_ascii_case("INFO"),
            Self::Warn => level.eq_ignore_ascii_case("WARN"),
            Self::Error => level.eq_ignore_ascii_case("ERROR"),
        }
    }
}

/// What the event loop should do after a key is handled.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Continue,
    Quit,
}

/// Upper bound on the interactive search buffer — a held key can't grow it
/// without limit (Power-of-10 rule 3).
const MAX_QUERY_LEN: usize = 128;

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
    /// Log search query (`/`). Empty = no text filter.
    pub log_query: String,
    /// True while the `/` search input is capturing keystrokes.
    pub log_input_active: bool,
    /// Log severity filter (`l`).
    pub log_level: LogLevelFilter,
    /// `?` help overlay is showing.
    pub show_help: bool,
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

    fn push_query_char(&mut self, c: char) {
        if self.log_query.len() < MAX_QUERY_LEN {
            self.log_query.push(c);
        }
    }

    /// Clear every log filter (route selection, text query, level) in one key.
    fn clear_log_filters(&mut self) {
        self.selected_route = None;
        self.log_query.clear();
        self.log_level = LogLevelFilter::All;
    }

    /// Handle one key. Returns `Quit` when the event loop should exit. This is
    /// the whole interaction model — kept here (not in the render thread) so it
    /// is unit-testable without a terminal.
    pub fn on_key(&mut self, code: KeyCode, ctrl: bool) -> KeyOutcome {
        // Ctrl-C/D/\ always quit, even mid-search — in raw mode the terminal
        // doesn't translate them to signals, so we must.
        if ctrl && matches!(code, KeyCode::Char('c' | 'd' | '\\')) {
            return KeyOutcome::Quit;
        }

        // Search input mode captures keystrokes before any command binding, so
        // typing `q`/`j`/`/` edits the query instead of triggering commands.
        if self.log_input_active {
            match code {
                KeyCode::Char(c) => self.push_query_char(c),
                KeyCode::Backspace => {
                    self.log_query.pop();
                }
                KeyCode::Enter => self.log_input_active = false, // apply, keep query
                KeyCode::Esc => {
                    self.log_input_active = false;
                    self.log_query.clear();
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        // Help overlay is modal: any key dismisses it.
        if self.show_help {
            self.show_help = false;
            return KeyOutcome::Continue;
        }

        match code {
            KeyCode::Char('q') => return KeyOutcome::Quit,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('/') => self.log_input_active = true,
            KeyCode::Char('l') => self.log_level = self.log_level.next(),
            KeyCode::Char('c') => self.clear_log_filters(),
            KeyCode::Esc => {
                // Back out one level: query → level → selection → quit.
                if !self.log_query.is_empty() {
                    self.log_query.clear();
                } else if self.log_level != LogLevelFilter::All {
                    self.log_level = LogLevelFilter::All;
                } else if self.selected_route.is_some() {
                    self.selected_route = None;
                } else {
                    return KeyOutcome::Quit;
                }
            }
            KeyCode::Tab | KeyCode::Right => self.next_tab(),
            KeyCode::BackTab | KeyCode::Left => self.prev_tab(),
            KeyCode::Down | KeyCode::Char('j') if self.selected_tab == 0 => {
                self.select_next_route()
            }
            KeyCode::Up | KeyCode::Char('k') if self.selected_tab == 0 => self.select_prev_route(),
            _ => {}
        }
        KeyOutcome::Continue
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
    fn slash_enters_search_and_typing_builds_query() {
        let mut app = App::default();
        assert_eq!(app.on_key(KeyCode::Char('/'), false), KeyOutcome::Continue);
        assert!(app.log_input_active);
        // Command chars are captured as text while typing, not executed.
        for c in ['q', 'i', 'n', 'g'] {
            app.on_key(KeyCode::Char(c), false);
        }
        assert_eq!(app.log_query, "qing");
        app.on_key(KeyCode::Backspace, false);
        assert_eq!(app.log_query, "qin");
        // Enter applies and exits input mode, keeping the query.
        assert_eq!(app.on_key(KeyCode::Enter, false), KeyOutcome::Continue);
        assert!(!app.log_input_active);
        assert_eq!(app.log_query, "qin");
    }

    #[test]
    fn esc_in_search_cancels_and_clears_query() {
        let mut app = App::default();
        app.on_key(KeyCode::Char('/'), false);
        app.on_key(KeyCode::Char('x'), false);
        app.on_key(KeyCode::Esc, false);
        assert!(!app.log_input_active);
        assert_eq!(app.log_query, "");
    }

    #[test]
    fn q_while_typing_does_not_quit() {
        let mut app = App::default();
        app.on_key(KeyCode::Char('/'), false);
        assert_eq!(app.on_key(KeyCode::Char('q'), false), KeyOutcome::Continue);
        assert_eq!(app.log_query, "q");
    }

    #[test]
    fn l_cycles_level_filter() {
        let mut app = App::default();
        assert_eq!(app.log_level, LogLevelFilter::All);
        app.on_key(KeyCode::Char('l'), false);
        assert_eq!(app.log_level, LogLevelFilter::Info);
        app.on_key(KeyCode::Char('l'), false);
        assert_eq!(app.log_level, LogLevelFilter::Warn);
        app.on_key(KeyCode::Char('l'), false);
        assert_eq!(app.log_level, LogLevelFilter::Error);
        app.on_key(KeyCode::Char('l'), false);
        assert_eq!(app.log_level, LogLevelFilter::All, "wraps back to All");
    }

    #[test]
    fn help_toggles_and_any_key_dismisses() {
        let mut app = App::default();
        app.on_key(KeyCode::Char('?'), false);
        assert!(app.show_help);
        // While help is open, a normally-quitting key just dismisses.
        assert_eq!(app.on_key(KeyCode::Char('q'), false), KeyOutcome::Continue);
        assert!(!app.show_help);
    }

    #[test]
    fn esc_backs_out_one_level_then_quits() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(1);
        app.log_level = LogLevelFilter::Warn;
        app.log_query = "boom".into();
        // query → level → selection → quit
        assert_eq!(app.on_key(KeyCode::Esc, false), KeyOutcome::Continue);
        assert_eq!(app.log_query, "");
        assert_eq!(app.on_key(KeyCode::Esc, false), KeyOutcome::Continue);
        assert_eq!(app.log_level, LogLevelFilter::All);
        assert_eq!(app.on_key(KeyCode::Esc, false), KeyOutcome::Continue);
        assert_eq!(app.selected_route, None);
        assert_eq!(app.on_key(KeyCode::Esc, false), KeyOutcome::Quit);
    }

    #[test]
    fn c_clears_all_log_filters() {
        let mut app = app_with_routes(3);
        app.selected_route = Some(2);
        app.log_query = "x".into();
        app.log_level = LogLevelFilter::Error;
        app.on_key(KeyCode::Char('c'), false);
        assert_eq!(app.selected_route, None);
        assert_eq!(app.log_query, "");
        assert_eq!(app.log_level, LogLevelFilter::All);
    }

    #[test]
    fn ctrl_c_and_q_quit() {
        let mut app = App::default();
        assert_eq!(app.on_key(KeyCode::Char('c'), true), KeyOutcome::Quit);
        assert_eq!(app.on_key(KeyCode::Char('q'), false), KeyOutcome::Quit);
    }

    #[test]
    fn query_length_is_bounded() {
        let mut app = App::default();
        app.on_key(KeyCode::Char('/'), false);
        for _ in 0..(MAX_QUERY_LEN + 50) {
            app.on_key(KeyCode::Char('a'), false);
        }
        assert_eq!(
            app.log_query.len(),
            MAX_QUERY_LEN,
            "held key cannot overflow"
        );
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
