use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info};
use crate::config::{Config, FunctionConfig};
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
        tokio::time::sleep(Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        match Config::from_file(&path) {
            Ok(new_config) => {
                if let Err(e) = new_config.validate() {
                    error!("config reload rejected: {e}");
                    continue;
                }
                info!("config reloaded: {} functions", new_config.functions.len());

                let old_config = state.config.read().await.clone();
                let old_funcs = &old_config.functions;
                let new_funcs = &new_config.functions;

                // Removed: drain pool entirely.
                for name in old_funcs.keys() {
                    if !new_funcs.contains_key(name) {
                        info!("hot-reload: removing function {name}");
                        state.process_manager.drain_pool(name).await;
                    }
                }

                // Changed: hot_swap the existing pool.
                for (name, new_cfg) in new_funcs {
                    if let Some(old_cfg) = old_funcs.get(name) {
                        if function_changed(old_cfg, new_cfg) {
                            info!("hot-reload: swapping pool for {name}");
                            if let Err(e) = state.process_manager.hot_swap(
                                name,
                                new_cfg.clone(),
                                &*state.runtime_registry,
                            ).await {
                                error!("hot_swap failed for {name}: {e}");
                            }
                        }
                    }
                }

                // New: spawn fresh pool.
                let log_tx = state.log_tx.clone();
                for (name, new_cfg) in new_funcs {
                    if !old_funcs.contains_key(name) {
                        info!("hot-reload: adding function {name}");
                        if let Err(e) = state.process_manager.spawn_function(
                            name, new_cfg, &state.runtime_registry, log_tx.clone(),
                        ).await {
                            error!("spawn_function failed for {name}: {e}");
                        }
                    }
                }

                // Rebuild handler list — one ProcessHandler per function.
                // System handlers retained from current Router (their Arc is
                // re-mounted unchanged).
                let mut handlers: Vec<Arc<dyn crate::runtime::LambdaHandler>> = Vec::new();
                {
                    let current = state.router.read().await;
                    for h in current.handlers() {
                        // System handlers have names starting with "_riz" or
                        // match the system-function route shapes. We keep
                        // them by checking they're not in new_funcs.
                        let name = h.name();
                        if !new_funcs.contains_key(name) && !old_funcs.contains_key(name) {
                            handlers.push(h.clone());
                        }
                    }
                }
                for (name, cfg) in new_funcs {
                    let h = crate::runtime::process::ProcessHandler::for_function(
                        name, cfg, state.process_manager.clone(),
                    );
                    handlers.push(Arc::new(h));
                }

                // Re-register any new functions in RizState (preserves counters
                // for already-registered names via IndexMap::insert overwrite —
                // but we want to preserve counters, so only register names not
                // already present).
                {
                    let known: std::collections::HashSet<String> = state.riz_state.functions
                        .read().await.keys().cloned().collect();
                    for (name, cfg) in new_funcs {
                        if !known.contains(name) {
                            state.riz_state.register(
                                crate::state::FunctionState::user(name.clone(), cfg.clone())
                            ).await;
                        }
                    }
                }

                let new_router = Router::new(handlers);
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

fn function_changed(old: &FunctionConfig, new: &FunctionConfig) -> bool {
    old.handler != new.handler
        || old.concurrency != new.concurrency
        || old.timeout_ms != new.timeout_ms
        || old.runtime != new.runtime
        || old.routes.len() != new.routes.len()
        || old.routes.iter().zip(new.routes.iter())
            .any(|(a, b)| a.path != b.path || a.method != b.method)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FunctionConfig, RouteSpec, RuntimeKind};
    use std::path::PathBuf;

    fn make_cfg(handler: &str, concurrency: usize) -> FunctionConfig {
        FunctionConfig {
            runtime: RuntimeKind::Bun,
            protocol: Default::default(),
            handler: PathBuf::from(handler),
            timeout_ms: 5000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            cache_ttl_secs: None,
            concurrency,
            routes: vec![],
        }
    }

    #[test]
    fn hotreload_picks_up_riz_toml_changes() {
        let r1 = make_cfg("./old.ts", 1);
        let r2 = make_cfg("./new.ts", 1);
        assert!(function_changed(&r1, &r2));
    }

    #[test]
    fn function_changed_detects_concurrency_change() {
        let r1 = make_cfg("./same.ts", 1);
        let r2 = make_cfg("./same.ts", 2);
        assert!(function_changed(&r1, &r2));
    }

    #[test]
    fn function_unchanged_when_identical() {
        let r1 = make_cfg("./same.ts", 1);
        let r2 = make_cfg("./same.ts", 1);
        assert!(!function_changed(&r1, &r2));
    }

    #[test]
    fn function_changed_detects_route_change() {
        let mut r1 = make_cfg("./same.ts", 1);
        r1.routes = vec![RouteSpec { path: "/a".into(), method: "GET".into() }];
        let mut r2 = make_cfg("./same.ts", 1);
        r2.routes = vec![RouteSpec { path: "/b".into(), method: "GET".into() }];
        assert!(function_changed(&r1, &r2));
    }
}
