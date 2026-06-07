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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Set to true by the server's shutdown path. The TUI loop checks this on
/// every tick and exits cleanly — running its cleanup before the main thread
/// returns and kills the detached TUI thread.
///
/// Without this signal, an external Ctrl-C / SIGTERM left the terminal in
/// raw mode + mouse-capture mode, which prints raw escape sequences
/// (35;76;22M...) on every keystroke after riz exits.
pub static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Signal the TUI to exit cleanly. Called from main.rs on graceful shutdown.
/// Idempotent and safe to call from any thread.
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Restore the terminal to its pre-TUI state. Idempotent — calling it twice
/// (e.g. from both the normal exit path AND the panic hook) is fine. Each
/// crossterm call returns Ok on a no-op; we ignore errors because by the
/// time this runs, we've usually already lost the chance to report them.
///
/// Writes the restore sequence to BOTH stdout and /dev/tty (best-effort).
/// stdout works in the normal happy path; /dev/tty is the belt-and-suspenders
/// path that works even if stdout has been redirected, closed by a parent
/// process exiting, or attached to a pipe that no longer leads to the user's
/// terminal. Without /dev/tty fallback, a process killed mid-render (or
/// where the parent shell closed the pipe before the cleanup ran) leaves
/// the terminal in raw mode + mouse-capture mode — and at that point the
/// user can't even kill the orphan because their keystrokes are intercepted.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    let _ = execute!(stdout, crossterm::cursor::Show);
    // Belt-and-suspenders: /dev/tty is the controlling terminal regardless
    // of how stdout was set up. On macOS + Linux this is the reliable path.
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = execute!(tty, LeaveAlternateScreen, DisableMouseCapture);
        let _ = execute!(tty, crossterm::cursor::Show);
    }
}

/// RAII guard. Restores the terminal when dropped, including during a
/// panic unwind. Belt and suspenders to the explicit cleanup paths +
/// the panic hook — there is no exit path from `run_tui_with_watch`
/// that doesn't restore terminal state, even if a panic happens after
/// the closure returns but before the explicit `restore_terminal()`.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Install a global panic hook that restores the terminal before delegating
/// to the original hook. Without this, a panic anywhere in the TUI thread
/// (or anywhere in the process, since the hook is global) leaves the user's
/// shell in raw mode + alt-screen + mouse-capture mode.
///
/// Only takes effect the first time it's called — subsequent calls are
/// no-ops so multiple TUI invocations in tests don't stack hooks.
fn install_panic_hook() {
    use std::sync::Once;
    static HOOK_INSTALLED: Once = Once::new();
    HOOK_INSTALLED.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            original(info);
        }));
    });
}

pub fn run_tui(state: Arc<AppState>, handle: tokio::runtime::Handle) -> anyhow::Result<()> {
    // Spawn the async snapshotter on the shared tokio runtime before entering
    // raw mode so the first snapshot is available (or at worst a default) by
    // the first tick.
    let watch_rx = snapshot::spawn_snapshotter(state, &handle);
    let result = run_tui_with_watch(watch_rx);
    // If the TUI exited under its own steam (user hit q / Ctrl-C / Esc /
    // Ctrl-D), the server is still running in the tokio main. Without
    // this, the user has to hit Ctrl-C a SECOND time from the now-headless
    // shell to actually terminate riz. Sending SIGTERM to ourselves
    // engages the existing graceful-shutdown path.
    //
    // Skipped when SHUTDOWN_REQUESTED is already set — that means the
    // server initiated the shutdown and the TUI broke its loop in
    // response; the server is already on its way out.
    if !SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::this(), Signal::SIGTERM);
        }
    }
    result
}

pub fn run_tui_with_watch(watch_rx: watch::Receiver<TuiSnapshot>) -> anyhow::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    // Drop guard. From here onward, ANY exit path from this function — normal
    // return, anyhow::Result::Err, panic unwind, thread cancellation — runs
    // restore_terminal() via the guard's Drop.
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();

    // Everything after enable_raw_mode must clean up raw mode on any error
    let result: anyhow::Result<()> = (|| {
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        // Force an initial clear so the first frame sizes correctly. Without
        // this, some terminals (alacritty, iTerm2 with split-pane) draw the
        // first frame with a stale size cached at Terminal::new() time and
        // the layout is broken until the user resizes the window.
        terminal.clear()?;
        let r = run_loop(&mut terminal, watch_rx);
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;
        r
    })();

    // Belt-and-suspenders: restore_terminal does the same cleanup as the
    // closure above. If the closure returned Err and skipped the cleanup
    // path, this still runs.
    restore_terminal();
    let _ = disable_raw_mode();
    result
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    watch_rx: watch::Receiver<TuiSnapshot>,
) -> anyhow::Result<()> {
    let mut app = App::default();
    let tick = Duration::from_millis(100);

    loop {
        // External-shutdown check (Ctrl-C, SIGTERM, server-side graceful
        // shutdown). Without this, the TUI thread runs until the main
        // thread exits and kills it — bypassing the cleanup path.
        if SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            break;
        }
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
                // In raw mode the terminal does NOT translate Ctrl-C to
                // SIGINT — it arrives as a key event. We have to handle it
                // ourselves or the user is trapped: every Ctrl-C they hit
                // is silently intercepted and the process can't be killed
                // from the controlling terminal. Same for Ctrl-D / Ctrl-\.
                use crossterm::event::KeyModifiers;
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl
                    && matches!(
                        key.code,
                        KeyCode::Char('c') | KeyCode::Char('d') | KeyCode::Char('\\')
                    )
                {
                    break;
                }
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
                    // `c` (without Ctrl) clears the route filter (mnemonic: "clear")
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
