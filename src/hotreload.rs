use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info};
use crate::config::Config;
use crate::router::Router;
use crate::state::AppState;

pub async fn watch_config(config_path: String, state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::channel::<()>(4);

    let path = config_path.clone();
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.try_send(());
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            error!("failed to create config watcher: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(Path::new(&config_path), RecursiveMode::NonRecursive) {
        error!("failed to watch {config_path}: {e}");
        return;
    }

    info!("watching {config_path} for changes");

    loop {
        if rx.recv().await.is_none() {
            break;
        }
        // Debounce: wait for quiet period before reloading
        tokio::time::sleep(Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        match Config::from_file(&path) {
            Ok(new_config) => {
                info!("config reloaded: {} routes", new_config.routes.len());
                let new_router = Router::new(new_config.routes.clone());
                *state.router.write().await = new_router;
                *state.config.write().await = new_config;
            }
            Err(e) => {
                error!("config reload failed (keeping current): {e}");
            }
        }
    }

    drop(watcher);
}
