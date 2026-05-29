use crate::config::{Config, FunctionConfig};
use crate::router::Router;
use crate::state::AppState;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info};

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
                let new_stage = new_config.server.stage.clone();
                let new_default_ttl = new_config.cache.default_ttl_secs;

                // Removed: drain pool entirely.
                for name in old_funcs.keys() {
                    if !new_funcs.contains_key(name) {
                        info!("hot-reload: removing function {name}");
                        state.process_manager.drain_pool(name).await;
                    }
                }

                // Changed: hot_swap the existing pool + update FunctionState metadata.
                for (name, new_cfg) in new_funcs {
                    if let Some(old_cfg) = old_funcs.get(name) {
                        // Always update cached metadata — even if the pool didn't
                        // change, cache_ttl_secs or stage might have.
                        {
                            let functions = state.riz_state.functions.read().await;
                            if let Some(fs) = functions.get(name) {
                                fs.update_metadata(new_cfg, &new_stage, new_default_ttl);
                            }
                        }
                        if function_changed(old_cfg, new_cfg) {
                            info!("hot-reload: swapping pool for {name}");
                            if let Err(e) = state
                                .process_manager
                                .hot_swap(name, new_cfg.clone(), &state.runtime_registry)
                                .await
                            {
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
                        if let Err(e) = state
                            .process_manager
                            .spawn_function(name, new_cfg, &state.runtime_registry, log_tx.clone())
                            .await
                        {
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
                        name,
                        cfg,
                        state.process_manager.clone(),
                    );
                    handlers.push(Arc::new(h));
                }

                // Re-register any new functions in RizState (preserves counters
                // for already-registered names via IndexMap::insert overwrite —
                // but we want to preserve counters, so only register names not
                // already present).
                {
                    let known: std::collections::HashSet<String> = state
                        .riz_state
                        .functions
                        .read()
                        .await
                        .keys()
                        .cloned()
                        .collect();
                    for (name, cfg) in new_funcs {
                        if !known.contains(name) {
                            state
                                .riz_state
                                .register(crate::state::FunctionState::user(
                                    name.clone(),
                                    cfg.clone(),
                                    &new_stage,
                                    new_default_ttl,
                                ))
                                .await;
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

/// Watch each function's handler directory and hot-swap its pool when a
/// source file changes. Closes the day-to-day DX gap: previously, editing
/// `index.ts` required touching `riz.toml` to trigger a reload.
///
/// Non-recursive: only files at the immediate handler-directory level
/// trigger. Deep imports (e.g. `lib/utils.ts` imported from
/// `index.ts`) need a manual touch on the handler file. Future
/// enhancement: opt-in recursive mode per function, with sensible
/// ignore patterns for `node_modules/` / `target/` / `__pycache__/`.
///
/// Debounce window is 200 ms (matches `watch_config`). Coalesces
/// bursts (save → linter rewrite → save again) into one hot-swap.
pub async fn watch_handler_sources(state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::channel::<PathBuf>(64);

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                if let Some(p) = event.paths.into_iter().next() {
                    let _ = tx.try_send(p);
                }
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            error!("failed to create handler watcher: {e}");
            return;
        }
    };

    // Snapshot the handler-dir → function-name map at startup.
    // Hot-reload of riz.toml that ADDS new functions doesn't re-register
    // them in this watcher (v1 limitation; revisit if needed).
    //
    // Canonicalize the dir paths so the prefix check below works on macOS,
    // where `/var/folders/...` symlinks to `/private/var/folders/...` and
    // notify events come back as the canonical resolved path. Without
    // this, `event_path.starts_with(watched_dir)` silently misses.
    let dirs_to_function: HashMap<PathBuf, String> = {
        let cfg = state.config.read().await;
        cfg.functions
            .iter()
            .filter_map(|(name, fcfg)| {
                let dir = fcfg.handler.parent()?.to_path_buf();
                if dir.as_os_str().is_empty() {
                    return None;
                }
                let canonical = dir.canonicalize().unwrap_or(dir);
                Some((canonical, name.clone()))
            })
            .collect()
    };
    for dir in dirs_to_function.keys() {
        // Recursive on purpose: macOS FSEvents NonRecursive only fires
        // events on the directory itself (rename/delete of the dir),
        // not on files INSIDE. Recursive picks up nested files on both
        // mac and Linux. Trade-off: stray writes deep inside a handler
        // dir (cargo build artifacts, node_modules touches) can spam
        // hot-swaps. Document this; future enhancement is ignore patterns.
        if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
            error!(
                "failed to watch handler dir {}: {e}",
                dir.display()
            );
        } else {
            info!("watching handler dir {} for source changes", dir.display());
        }
    }

    loop {
        let first_path = match rx.recv().await {
            Some(p) => p,
            None => break,
        };
        // Debounce: coalesce bursts within 200 ms.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut paths_seen = vec![first_path];
        while let Ok(p) = rx.try_recv() {
            paths_seen.push(p);
        }

        // Re-read the dir map in case riz.toml hot-reload changed it.
        // Canonicalize for the same reason as the initial snapshot above.
        let dirs_now: HashMap<PathBuf, String> = {
            let cfg = state.config.read().await;
            cfg.functions
                .iter()
                .filter_map(|(name, fcfg)| {
                    let dir = fcfg.handler.parent()?.to_path_buf();
                    if dir.as_os_str().is_empty() {
                        return None;
                    }
                    let canonical = dir.canonicalize().unwrap_or(dir);
                    Some((canonical, name.clone()))
                })
                .collect()
        };

        let mut functions_to_swap: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for changed in &paths_seen {
            for (dir, fn_name) in &dirs_now {
                if changed.starts_with(dir) {
                    functions_to_swap.insert(fn_name.clone());
                    break;
                }
            }
        }

        for fn_name in functions_to_swap {
            let fcfg_opt = {
                let cfg = state.config.read().await;
                cfg.functions.get(&fn_name).cloned()
            };
            if let Some(fcfg) = fcfg_opt {
                info!("handler source change → hot-swap {fn_name}");
                if let Err(e) = state
                    .process_manager
                    .hot_swap(&fn_name, fcfg, &state.runtime_registry)
                    .await
                {
                    error!("hot_swap on source change failed for {fn_name}: {e}");
                }
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
        || old
            .routes
            .iter()
            .zip(new.routes.iter())
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
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
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
        r1.routes = vec![RouteSpec {
            path: "/a".into(),
            method: "GET".into(),
        }];
        let mut r2 = make_cfg("./same.ts", 1);
        r2.routes = vec![RouteSpec {
            path: "/b".into(),
            method: "GET".into(),
        }];
        assert!(function_changed(&r1, &r2));
    }
}
