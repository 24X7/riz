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

    let Some(watcher) = create_config_watcher(&config_path, tx) else {
        return;
    };

    loop {
        if rx.recv().await.is_none() {
            break;
        }
        // Debounce: coalesce bursts within 200 ms.
        tokio::time::sleep(Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}
        reload_config_from_file(&config_path, &state).await;
    }

    drop(watcher);
}

/// Event intake for `watch_config`: create the notify watcher (its callback
/// pushes coalescible ticks onto the bounded channel) and point it at the
/// config file. `None` means watching is unavailable — the caller gives up
/// (the error is already logged).
fn create_config_watcher(
    config_path: &str,
    tx: mpsc::Sender<()>,
) -> Option<notify::RecommendedWatcher> {
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
            return None;
        }
    };

    if let Err(e) = watcher.watch(Path::new(config_path), RecursiveMode::NonRecursive) {
        error!("failed to watch {config_path}: {e}");
        return None;
    }

    info!("watching {config_path} for changes");
    Some(watcher)
}

/// Re-parse the config file and apply it when it parses AND validates; a
/// bad file keeps the current config running (log, don't crash the loop).
async fn reload_config_from_file(config_path: &str, state: &Arc<AppState>) {
    match Config::from_file(config_path) {
        Ok(new_config) => {
            if let Err(e) = new_config.validate() {
                error!("config reload rejected: {e}");
                return;
            }
            info!("config reloaded: {} functions", new_config.functions.len());
            apply_config_reload(state, new_config).await;
        }
        Err(e) => {
            error!("config reload failed (keeping current): {e}");
        }
    }
}

/// Apply a validated reloaded config: reconcile the process pools
/// (remove / swap / add), rebuild the router's handler list, register any
/// new functions in RizState, then swap the router and config in place.
async fn apply_config_reload(state: &Arc<AppState>, new_config: Config) {
    let old_config = state.config.read().await.clone();
    let old_funcs = &old_config.functions;
    let new_funcs = &new_config.functions;
    let new_stage = new_config.server.stage.clone();
    let new_default_ttl = new_config.cache.default_ttl_secs;

    drain_removed_functions(state, old_funcs, new_funcs).await;
    sync_existing_functions(state, old_funcs, new_funcs, &new_stage, new_default_ttl).await;
    spawn_added_functions(state, old_funcs, new_funcs).await;
    let handlers = rebuild_handler_list(state, old_funcs, new_funcs).await;
    register_new_function_states(state, new_funcs, &new_stage, new_default_ttl).await;

    let new_router = Router::new(handlers);
    // Rebuild the data-plane API-key gate from the new [api_keys] set. A fresh
    // limiter resets every caller's token budget — benign, reload is a rare
    // admin action — and keeps the caller set bounded to what config declares.
    let new_rate_limiter = crate::auth::api_key::RateLimiter::from_config(&new_config.api_keys);

    // Audit the applied change (counts only — never config contents). Computed
    // here while both function maps are still borrowable, before new_config moves.
    let removed = old_funcs
        .keys()
        .filter(|k| !new_funcs.contains_key(*k))
        .count();
    let added = new_funcs
        .keys()
        .filter(|k| !old_funcs.contains_key(*k))
        .count();
    let changed = new_funcs
        .iter()
        .filter(|(n, c)| old_funcs.get(*n).is_some_and(|o| function_changed(o, c)))
        .count();
    crate::audit::config_reload(added, removed, changed);

    *state.router.write().await = new_router;
    *state.rate_limiter.write().await = new_rate_limiter;
    *state.config.write().await = new_config;
}

/// Removed functions: drain each pool entirely.
async fn drain_removed_functions(
    state: &Arc<AppState>,
    old_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_funcs: &indexmap::IndexMap<String, FunctionConfig>,
) {
    for name in old_funcs.keys() {
        if !new_funcs.contains_key(name) {
            info!("hot-reload: removing function {name}");
            state.process_manager.drain_pool(name).await;
        }
    }
}

/// Changed functions: hot_swap the existing pool + update FunctionState
/// metadata.
async fn sync_existing_functions(
    state: &Arc<AppState>,
    old_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_stage: &str,
    new_default_ttl: u64,
) {
    for (name, new_cfg) in new_funcs {
        if let Some(old_cfg) = old_funcs.get(name) {
            // Always update cached metadata — even if the pool didn't
            // change, cache_ttl_secs or stage might have.
            {
                let functions = state.riz_state.functions.read().await;
                if let Some(fs) = functions.get(name) {
                    fs.update_metadata(new_cfg, new_stage, new_default_ttl);
                }
            }
            if function_changed(old_cfg, new_cfg) {
                info!("hot-reload: swapping pool for {name}");
                if let Err(e) = state.process_manager.hot_swap(name, new_cfg.clone()).await {
                    error!("hot_swap failed for {name}: {e}");
                }
            }
        }
    }
}

/// New functions: spawn a fresh pool for each.
async fn spawn_added_functions(
    state: &Arc<AppState>,
    old_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_funcs: &indexmap::IndexMap<String, FunctionConfig>,
) {
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
}

/// Rebuild the handler list — one ProcessHandler per function. System
/// handlers are retained from the current Router (their Arc is re-mounted
/// unchanged).
async fn rebuild_handler_list(
    state: &Arc<AppState>,
    old_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_funcs: &indexmap::IndexMap<String, FunctionConfig>,
) -> Vec<Arc<dyn crate::runtime::LambdaHandler>> {
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
    handlers
}

/// Re-register any new functions in RizState (preserves counters for
/// already-registered names via IndexMap::insert overwrite — but we want to
/// preserve counters, so only register names not already present).
async fn register_new_function_states(
    state: &Arc<AppState>,
    new_funcs: &indexmap::IndexMap<String, FunctionConfig>,
    new_stage: &str,
    new_default_ttl: u64,
) {
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
                    new_stage,
                    new_default_ttl,
                ))
                .await;
        }
    }
}

/// Path segments that are silently ignored by the handler-source watcher.
/// Any change whose path contains one of these as a directory component is
/// dropped before it can trigger a hot-swap. Without this, a single
/// `cargo build` or `npm install` floods the watcher with thousands of
/// events and forces a pointless pool restart.
///
/// The match is exact-segment, not substring: a directory literally named
/// `target` matches; a file like `targeting.ts` does not.
pub(crate) const IGNORE_SEGMENTS: &[&str] = &[
    "node_modules", // npm / bun / yarn
    "target",       // cargo build artifacts
    "__pycache__",  // python bytecode
    ".git",         // VCS
    ".venv",        // python venv
    "venv",         // python venv (alt)
    "dist",         // common bundler output
    "build",        // common build output
    ".next",        // next.js
    ".cache",       // generic build cache
];

/// Returns true if any directory component of `p` literally matches one of
/// the entries in `IGNORE_SEGMENTS`. Used to drop irrelevant filesystem
/// events before they can spam hot-swaps.
pub(crate) fn is_ignored_path(p: &std::path::Path) -> bool {
    p.components().any(|c| {
        if let std::path::Component::Normal(seg) = c {
            if let Some(s) = seg.to_str() {
                return IGNORE_SEGMENTS.contains(&s);
            }
        }
        false
    })
}

/// Watch each function's handler directory and hot-swap its pool when a
/// source file changes. Closes the day-to-day DX gap: previously, editing
/// `index.ts` required touching `riz.toml` to trigger a reload.
///
/// Recursive watch on the handler dir, with `IGNORE_SEGMENTS` filtering
/// out generated/vendored directories so a `cargo build` or `npm install`
/// doesn't spam hot-swaps.
///
/// Debounce window is 200 ms (matches `watch_config`). Coalesces
/// bursts (save → linter rewrite → save again) into one hot-swap.
pub async fn watch_handler_sources(state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::channel::<PathBuf>(64);

    let Some(mut watcher) = create_source_watcher(tx) else {
        return;
    };

    // Snapshot the handler-dir → function-name map at startup.
    // Hot-reload of riz.toml that ADDS new functions doesn't re-register
    // them in this watcher (v1 limitation; revisit if needed).
    let dirs_to_function = snapshot_handler_dirs(&state).await;
    watch_handler_dirs(&mut watcher, &dirs_to_function);

    loop {
        let Some(first_path) = rx.recv().await else {
            break;
        };
        let paths_seen = collect_debounced_paths(&mut rx, first_path).await;
        if paths_seen.is_empty() {
            continue;
        }

        // Re-read the dir map in case riz.toml hot-reload changed it.
        let dirs_now = snapshot_handler_dirs(&state).await;
        let functions_to_swap = functions_owning_paths(&paths_seen, &dirs_now);
        hot_swap_functions(&state, functions_to_swap).await;
    }
    drop(watcher);
}

/// Event intake for `watch_handler_sources`: create the notify watcher whose
/// callback pushes changed paths onto the bounded channel. `None` means
/// watching is unavailable — the caller gives up (already logged).
fn create_source_watcher(tx: mpsc::Sender<PathBuf>) -> Option<notify::RecommendedWatcher> {
    match notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                if let Some(p) = event.paths.into_iter().next() {
                    // Filter at the channel boundary so ignored paths don't
                    // even consume buffer slots — a single `cargo build`
                    // can produce thousands of events.
                    if !is_ignored_path(&p) {
                        let _ = tx.try_send(p);
                    }
                }
            }
        }
    }) {
        Ok(w) => Some(w),
        Err(e) => {
            error!("failed to create handler watcher: {e}");
            None
        }
    }
}

/// Register every handler dir with the watcher. A dir that fails to watch is
/// logged and skipped — the others still hot-swap.
fn watch_handler_dirs(watcher: &mut notify::RecommendedWatcher, dirs: &HashMap<PathBuf, String>) {
    for dir in dirs.keys() {
        // Recursive on purpose: macOS FSEvents NonRecursive only fires
        // events on the directory itself (rename/delete of the dir),
        // not on files INSIDE. Recursive picks up nested files on both
        // mac and Linux. Trade-off: stray writes deep inside a handler
        // dir (cargo build artifacts, node_modules touches) can spam
        // hot-swaps. Document this; future enhancement is ignore patterns.
        if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
            error!("failed to watch handler dir {}: {e}", dir.display());
        } else {
            info!("watching handler dir {} for source changes", dir.display());
        }
    }
}

/// Debounce phase: after the first changed path arrives, wait out the 200 ms
/// window, drain everything else that queued up, and drop ignored paths.
/// (Defense-in-depth: the channel callback already filters, but if a future
/// caller pushes onto the channel directly, the same filtering applies here.)
async fn collect_debounced_paths(
    rx: &mut mpsc::Receiver<PathBuf>,
    first_path: PathBuf,
) -> Vec<PathBuf> {
    // Debounce: coalesce bursts within 200 ms.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut paths_seen = vec![first_path];
    while let Ok(p) = rx.try_recv() {
        paths_seen.push(p);
    }
    paths_seen.retain(|p| !is_ignored_path(p));
    paths_seen
}

/// Read the current handler-dir → function-name map from the live config.
///
/// Canonicalize the dir paths so the event-path prefix check works on macOS,
/// where `/var/folders/...` symlinks to `/private/var/folders/...` and
/// notify events come back as the canonical resolved path. Without
/// this, `event_path.starts_with(watched_dir)` silently misses.
async fn snapshot_handler_dirs(state: &Arc<AppState>) -> HashMap<PathBuf, String> {
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
}

/// Map a debounced batch of changed paths to the set of functions whose
/// handler dir contains at least one of them.
fn functions_owning_paths(
    paths_seen: &[PathBuf],
    dirs: &HashMap<PathBuf, String>,
) -> std::collections::HashSet<String> {
    let mut functions_to_swap: std::collections::HashSet<String> = std::collections::HashSet::new();
    for changed in paths_seen {
        for (dir, fn_name) in dirs {
            if changed.starts_with(dir) {
                functions_to_swap.insert(fn_name.clone());
                break;
            }
        }
    }
    functions_to_swap
}

/// Hot-swap each function's pool after a source change. A missing config
/// entry (function removed since the event fired) is silently skipped.
async fn hot_swap_functions(
    state: &Arc<AppState>,
    functions_to_swap: std::collections::HashSet<String>,
) {
    for fn_name in functions_to_swap {
        let fcfg_opt = {
            let cfg = state.config.read().await;
            cfg.functions.get(&fn_name).cloned()
        };
        if let Some(fcfg) = fcfg_opt {
            info!("handler source change → hot-swap {fn_name}");
            if let Err(e) = state.process_manager.hot_swap(&fn_name, fcfg).await {
                error!("hot_swap on source change failed for {fn_name}: {e}");
            }
        }
    }
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
            env: Default::default(),
            cache_ttl_secs: None,
            concurrency,
            routes: vec![],
            cors: None,
            authorizer: None,
            memory_mb: None,
            cpu_time_secs: None,
            allowed_paths: None,
            mcp: None,
            capabilities: Default::default(),
            guard_in: None,
            guard_out: None,
        }
    }

    #[test]
    fn hotreload_picks_up_riz_toml_changes() {
        let r1 = make_cfg("./old.ts", 1);
        let r2 = make_cfg("./new.ts", 1);
        assert!(function_changed(&r1, &r2));
    }

    #[test]
    fn is_ignored_path_drops_known_generated_dirs() {
        let cases = [
            ("/proj/handler/node_modules/foo/bar.ts", true),
            ("/proj/handler/target/debug/build.rs", true),
            ("/proj/handler/__pycache__/main.cpython-311.pyc", true),
            ("/proj/handler/.git/HEAD", true),
            ("/proj/handler/.venv/lib/site.py", true),
            ("/proj/handler/dist/bundle.js", true),
            ("/proj/handler/build/out.wasm", true),
            ("/proj/handler/.next/static.js", true),
        ];
        for (p, want) in cases {
            assert_eq!(
                is_ignored_path(std::path::Path::new(p)),
                want,
                "is_ignored_path({p}) should be {want}"
            );
        }
    }

    #[test]
    fn is_ignored_path_allows_real_source_files() {
        let cases = [
            "/proj/handler/index.ts",
            "/proj/handler/src/main.rs",
            "/proj/handler/main.py",
            "/proj/handler/lib/util.py",
            "/proj/handler/Cargo.toml",
        ];
        for p in cases {
            assert!(
                !is_ignored_path(std::path::Path::new(p)),
                "is_ignored_path({p}) should be false — real source"
            );
        }
    }

    #[test]
    fn is_ignored_path_substring_match_does_not_trigger() {
        // A file literally named `targeting.ts` is NOT in a `target/` dir.
        // The match is exact-segment to avoid false positives like this.
        assert!(!is_ignored_path(std::path::Path::new(
            "/proj/handler/targeting.ts"
        )));
        assert!(!is_ignored_path(std::path::Path::new(
            "/proj/handler/build_helpers/main.ts"
        )));
        assert!(!is_ignored_path(std::path::Path::new(
            "/proj/handler/python_cache/main.py"
        )));
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
