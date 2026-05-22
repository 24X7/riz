pub mod app;
pub mod widgets;

use std::io;
use std::sync::Arc;
use std::time::Duration;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use crate::state::AppState;
use self::app::App;

pub fn run_tui(state: Arc<AppState>, handle: tokio::runtime::Handle) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    // Everything after enable_raw_mode must clean up raw mode on any error
    let result: anyhow::Result<()> = (|| {
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let r = run_loop(&mut terminal, state, handle);
        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
        terminal.show_cursor()?;
        r
    })();

    disable_raw_mode()?;
    result
}

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

        // Clamp selection if routes were removed
        if let Some(i) = app.selected_route {
            if i >= app.route_stats.len() {
                app.selected_route = app.route_stats.len().checked_sub(1);
            }
        }

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
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.selected_tab == 0 {
                            app.select_next_route();
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if app.selected_tab == 0 {
                            app.select_prev_route();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
