mod cache;
mod config;
mod deploy;
mod gateway;
mod hotreload;
mod metrics;
mod process;
mod router;
mod server;
mod state;
mod tui;

use std::net::SocketAddr;
use std::sync::Arc;
use clap::{Parser, Subcommand};
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
    Start,
    Validate,
    Routes,
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
}

fn effective_config_path(dev: bool, explicit: Option<&str>) -> String {
    explicit.map(|s| s.to_string()).unwrap_or_else(|| {
        if dev { "examples/riz.dev.toml".into() } else { "riz.toml".into() }
    })
}

fn effective_log_level<'a>(dev: bool, explicit: Option<&'a str>) -> &'a str {
    explicit.unwrap_or(if dev { "debug" } else { "info" })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config_path = effective_config_path(cli.dev, cli.config.as_deref());
    let log_level = effective_log_level(cli.dev, cli.log_level.as_deref());
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

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

    match &cli.command {
        Some(Commands::Validate) => {
            println!("Config OK: {} routes", config.routes.len());
            return Ok(());
        }
        Some(Commands::Routes) => {
            for route in &config.routes {
                println!("{} {} -> {:?} ({})",
                    route.method, route.path,
                    route.handler, route.runtime.as_str());
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
    let router = router::Router::new(config.routes.clone());
    let process_manager = process::ProcessManager::new();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<state::LogEntry>(10_000);

    if config.effective_deploy_key().is_none() {
        tracing::warn!("SECURITY: no deploy key configured — POST /deploy is unauthenticated");
    }

    process_manager.spawn_all(&config.routes, &registry, log_tx.clone()).await?;

    let app_state = Arc::new(state::AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
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
        let cli = Cli::try_parse_from(["osbox"]).unwrap();
        assert!(!cli.dev);
    }

    #[test]
    fn explicit_config_overrides_dev_default() {
        let cli = Cli::try_parse_from(["riz", "--dev", "--config", "custom.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("custom.toml"));
        assert_eq!(effective_config_path(cli.dev, cli.config.as_deref()), "custom.toml");
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
