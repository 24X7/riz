use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info};
use crate::config::{Config, RouteConfig};
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

                let old_config = state.config.read().await.clone();

                // Build lookup maps by route_key
                let old_routes: HashMap<String, RouteConfig> = old_config.routes.iter()
                    .map(|r| (Router::route_key(&r.method, &r.path), r.clone()))
                    .collect();
                let new_routes: HashMap<String, RouteConfig> = new_config.routes.iter()
                    .map(|r| (Router::route_key(&r.method, &r.path), r.clone()))
                    .collect();

                // Removed routes: drain their pools
                for key in old_routes.keys() {
                    if !new_routes.contains_key(key.as_str()) {
                        info!("hot-reload: removing pool for {key}");
                        state.process_manager.drain_pool(key).await;
                    }
                }

                // Changed routes: hot_swap
                for (key, new_route) in &new_routes {
                    if let Some(old_route) = old_routes.get(key.as_str()) {
                        if route_changed(old_route, new_route) {
                            info!("hot-reload: swapping pool for {key}");
                            if let Err(e) = state.process_manager.hot_swap(key, new_route.clone(), &*state.runtime_registry).await {
                                error!("hot_swap failed for {key}: {e}");
                            }
                        }
                    }
                }

                // New routes: spawn
                let log_tx = state.log_tx.clone();
                for (key, new_route) in &new_routes {
                    if !old_routes.contains_key(key.as_str()) {
                        info!("hot-reload: adding pool for {key}");
                        if let Err(e) = state.process_manager.spawn_route(new_route, &state.runtime_registry, log_tx.clone()).await {
                            error!("spawn_route failed for {key}: {e}");
                        }
                    }
                }

                // Update router and config last (after pools are ready)
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

fn route_changed(old: &RouteConfig, new: &RouteConfig) -> bool {
    old.handler != new.handler
        || old.concurrency != new.concurrency
        || old.timeout_ms != new.timeout_ms
        || old.runtime != new.runtime
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RouteConfig, RuntimeKind};
    use std::path::PathBuf;

    fn make_route(path: &str, handler: &str, concurrency: usize) -> RouteConfig {
        RouteConfig {
            path: path.into(),
            method: "GET".into(),
            runtime: RuntimeKind::Bun,
            handler: PathBuf::from(handler),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency,
        }
    }

    #[test]
    fn route_changed_detects_handler_change() {
        let r1 = make_route("/foo", "./old.ts", 1);
        let r2 = make_route("/foo", "./new.ts", 1);
        assert!(route_changed(&r1, &r2));
    }

    #[test]
    fn route_changed_detects_concurrency_change() {
        let r1 = make_route("/foo", "./same.ts", 1);
        let r2 = make_route("/foo", "./same.ts", 2);
        assert!(route_changed(&r1, &r2));
    }

    #[test]
    fn route_unchanged_when_identical() {
        let r1 = make_route("/foo", "./same.ts", 1);
        let r2 = make_route("/foo", "./same.ts", 1);
        assert!(!route_changed(&r1, &r2));
    }
}
