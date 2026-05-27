mod cache;
mod config;
mod deploy;
mod gateway;
mod hotreload;
mod metrics;
mod process;
mod router;
mod runtime;
mod server;
mod state;
mod system;
mod tui;
mod ws;

#[cfg(test)]
mod test_helpers;

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "riz", about = "Self-hosted AWS Lambda host")]
struct Cli {
    /// Config file. Defaults to riz.dev.toml in --dev mode, riz.toml otherwise.
    #[arg(short, long)]
    config: Option<String>,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long)]
    no_tui: bool,

    /// Log level. Defaults to debug in --dev mode, info otherwise.
    #[arg(long)]
    log_level: Option<String>,

    /// Developer mode: colorized logs, debug level, TUI always on, defaults to riz.dev.toml.
    #[arg(long)]
    dev: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the runtime. Default when no subcommand is given.
    Run,
    /// Validate riz.toml and exit.
    Validate,
    /// List configured functions and their routes.
    Routes,
    /// Hot-swap a deployed function from S3.
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
}

fn effective_config_path(dev: bool, explicit: Option<&str>) -> String {
    explicit.map(|s| s.to_string()).unwrap_or_else(|| {
        if dev {
            "examples/riz.dev.toml".into()
        } else {
            "riz.toml".into()
        }
    })
}

fn effective_log_level(dev: bool, explicit: Option<&str>) -> &str {
    explicit.unwrap_or(if dev { "debug" } else { "info" })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config_path = effective_config_path(cli.dev, cli.config.as_deref());
    let log_level = effective_log_level(cli.dev, cli.log_level.as_deref());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    if cli.dev {
        tracing_subscriber::fmt()
            .pretty()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    }

    let config = config::Config::from_file(&config_path)?;
    config
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;

    match &cli.command {
        Some(Commands::Validate) => {
            println!("Config OK: {} functions", config.functions.len());
            return Ok(());
        }
        Some(Commands::Routes) => {
            for (name, f) in &config.functions {
                let routes: Vec<String> = f
                    .effective_routes(name)
                    .into_iter()
                    .map(|r| format!("{} {}", r.method, r.path))
                    .collect();
                println!(
                    "{} [{}] {:?} ({})  routes: {}",
                    name,
                    f.runtime.as_str(),
                    f.handler,
                    f.runtime.as_str(),
                    routes.join(", ")
                );
            }
            return Ok(());
        }
        _ => {}
    }

    let port = cli.port.unwrap_or(config.server.port);
    let host: std::net::IpAddr = config.server.host.parse()?;
    let addr = SocketAddr::new(host, port);

    let registry = Arc::new(process::runtime::RuntimeRegistry::new()?);
    let cache = cache::CacheLayer::new(&config.cache);
    let metrics = metrics::MetricsEmitter::new(&config.datadog);
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<state::LogEntry>(10_000);

    let deploy_cfg = &config.deploy;
    if config.effective_deploy_key().is_none() && deploy_cfg.allowed_cidrs.is_empty() {
        tracing::error!("SECURITY: /deploy has no auth configured — endpoint will refuse all requests. Set RIZ_DEPLOY_KEY or deploy_key/allowed_cidrs in config.");
    }

    let riz_state = Arc::new(state::RizState::new());
    let stage = config.server.stage.clone();
    let default_ttl = config.cache.default_ttl_secs;
    // Register system endpoints first.
    riz_state
        .register(state::FunctionState::system(
            "_riz_health",
            vec!["GET /_riz/health".into()],
            &stage,
        ))
        .await;
    riz_state
        .register(state::FunctionState::system(
            "_riz_metrics",
            vec!["GET /_riz/metrics".into()],
            &stage,
        ))
        .await;
    riz_state
        .register(state::FunctionState::system(
            "_riz_registry",
            vec!["GET /_riz/registry".into()],
            &stage,
        ))
        .await;
    riz_state
        .register(state::FunctionState::system(
            "_riz_mcp",
            vec!["POST /_riz/mcp".into()],
            &stage,
        ))
        .await;
    // Register user functions by name.
    for (name, cfg) in &config.functions {
        riz_state
            .register(state::FunctionState::user(
                name.clone(),
                cfg.clone(),
                &stage,
                default_ttl,
            ))
            .await;
    }

    let process_manager = Arc::new(process::ProcessManager::new(riz_state.clone()));

    // Spawn one process pool per function. Each spawned process bumps
    // cold_starts on the matching FunctionState.
    process_manager
        .spawn_all(&config.functions, &registry, log_tx.clone())
        .await?;

    // Build the handler list. System handlers mount FIRST so /_riz/* always
    // beats any user attempt to shadow those paths.
    let ws_connections = ws::ConnectionStore::new();
    let mcp = Arc::new(system::mcp::McpHandler::new(riz_state.clone()));
    let mut handlers: Vec<Arc<dyn runtime::LambdaHandler>> = vec![
        Arc::new(system::health::HealthHandler::new(riz_state.clone())),
        Arc::new(system::metrics::MetricsHandler::new(riz_state.clone())),
        Arc::new(system::registry::RegistryHandler::new(riz_state.clone())),
        mcp.clone() as Arc<dyn runtime::LambdaHandler>,
        Arc::new(ws::management::ConnectionsHandler::new(
            ws_connections.clone(),
        )),
    ];
    // One ProcessHandler per HTTP function — it declares every route the
    // function serves (including implicit `ANY /<name>` when no routes block
    // is given). WebSocket functions are mounted as axum routes in build_app
    // (see src/server.rs) — they don't go through the LambdaHandler dispatch
    // path.
    for (name, cfg) in &config.functions {
        match cfg.protocol {
            config::Protocol::Http => {
                let h = runtime::process::ProcessHandler::for_function(
                    name,
                    cfg,
                    process_manager.clone(),
                );
                handlers.push(Arc::new(h));
            }
            config::Protocol::WebSocket => {
                // Mounted in build_app below; no LambdaHandler instance.
            }
        }
    }
    // McpHandler.tools_call needs an Arc<Router> for reentrant dispatch.
    // We construct the inner Router first, hand it to MCP, then wrap a clone
    // in AppState's RwLock so hot-reload can swap handler lists later.
    let router_arc = Arc::new(router::Router::new(handlers.clone()));
    mcp.set_router(router_arc.clone()).await;
    let router = router::Router::new(handlers);

    let app_state = Arc::new(state::AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections,
    });

    // Dev mode forces TUI on regardless of --no-tui and atty check.
    // Prod mode: TUI only if stdout is a TTY and --no-tui not set.
    let tui_enabled = if cli.dev {
        true
    } else {
        !cli.no_tui && std::io::IsTerminal::is_terminal(&std::io::stdout())
    };

    if tui_enabled {
        let tui_state = app_state.clone();
        let tui_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            if let Err(e) = tui::run_tui(tui_state, tui_handle) {
                eprintln!("TUI error: {e}");
            }
        });
    } else {
        // In headless mode, drain logs to tracing so the bounded channel doesn't back up.
        let state_for_drain = app_state.clone();
        tokio::spawn(async move {
            let mut rx = state_for_drain.log_rx.lock().await;
            while let Some(entry) = rx.recv().await {
                tracing::debug!(
                    route = entry.route_key.as_deref().unwrap_or("-"),
                    "[{}] {}",
                    entry.level,
                    entry.message
                );
            }
        });
    }

    let watch_state = app_state.clone();
    let watch_config_path = config_path.clone();
    tokio::spawn(async move {
        hotreload::watch_config(watch_config_path, watch_state).await;
    });

    if cli.dev {
        info!("riz starting in [dev] mode on {addr}");
    } else {
        info!(mode = "production", addr = %addr, "riz starting");
    }

    server::run(app_state, addr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_flag_parsed() {
        let cli = Cli::try_parse_from(["riz", "--dev"]).unwrap();
        assert!(cli.dev);
        assert!(cli.config.is_none());
        assert!(cli.log_level.is_none());
    }

    #[test]
    fn no_dev_flag_by_default() {
        let cli = Cli::try_parse_from(["riz"]).unwrap();
        assert!(!cli.dev);
    }

    #[test]
    fn explicit_config_overrides_dev_default() {
        let cli = Cli::try_parse_from(["riz", "--dev", "--config", "custom.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("custom.toml"));
        assert_eq!(
            effective_config_path(cli.dev, cli.config.as_deref()),
            "custom.toml"
        );
    }

    #[test]
    fn config_defaults_by_mode() {
        assert_eq!(effective_config_path(true, None), "examples/riz.dev.toml");
        assert_eq!(effective_config_path(false, None), "riz.toml");
    }

    #[test]
    fn log_level_defaults_by_mode() {
        assert_eq!(effective_log_level(true, None), "debug");
        assert_eq!(effective_log_level(false, None), "info");
        assert_eq!(effective_log_level(true, Some("warn")), "warn");
    }
}
