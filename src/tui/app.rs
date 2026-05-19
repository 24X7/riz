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
}

impl App {
    pub fn tab_titles() -> &'static [&'static str] {
        &["Routes", "Processes", "Cache", "Logs"]
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
}
