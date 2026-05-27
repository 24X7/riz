pub mod app;
pub mod snapshot;
pub mod widgets;

use self::app::App;
use self::snapshot::TuiSnapshot;
use crate::state::AppState;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

pub fn run_tui(state: Arc<AppState>, handle: tokio::runtime::Handle) -> anyhow::Result<()> {
    // Spawn the async snapshotter on the shared tokio runtime before entering
    // raw mode so the first snapshot is available (or at worst a default) by
    // the first tick.
    let watch_rx = snapshot::spawn_snapshotter(state, &handle);
    run_tui_with_watch(watch_rx)
}

pub fn run_tui_with_watch(watch_rx: watch::Receiver<TuiSnapshot>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    // Everything after enable_raw_mode must clean up raw mode on any error
    let result: anyhow::Result<()> = (|| {
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let r = run_loop(&mut terminal, watch_rx);
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;
        r
    })();

    disable_raw_mode()?;
    result
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    watch_rx: watch::Receiver<TuiSnapshot>,
) -> anyhow::Result<()> {
    let mut app = App::default();
    let tick = Duration::from_millis(100);

    loop {
        // Borrow the latest snapshot — no RwLock, no block_on.
        {
            let snap = watch_rx.borrow();
            app.function_stats = snap.functions.clone();
            app.pool_stats = snap.pool_stats.clone();
            app.host_stats = snap.host_stats.clone();
            app.uptime_secs = snap.uptime_secs;
            app.cache_entry_count = snap.cache_entry_count;
            app.log_entries = snap.log_entries.clone();
        }

        // Clamp selection if routes were removed
        if let Some(i) = app.selected_route {
            if i >= app.function_stats.len() {
                app.selected_route = app.function_stats.len().checked_sub(1);
            }
        }

        terminal.draw(|f| widgets::render(f, &app))?;

        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => {
                        // Esc backs out one level: clear route filter if any,
                        // otherwise quit. `q` always quits.
                        if app.selected_route.is_some() {
                            app.selected_route = None;
                        } else {
                            break;
                        }
                    }
                    // `c` always clears the route filter (mnemonic: "clear")
                    KeyCode::Char('c') => {
                        app.selected_route = None;
                    }
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
