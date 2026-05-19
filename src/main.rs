use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "osbox", about = "Self-hosted AWS Lambda host")]
struct Cli {
    #[arg(short, long, default_value = "osbox.toml")]
    config: String,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long)]
    no_tui: bool,

    #[arg(long, default_value = "info")]
    log_level: String,

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log_level))
        )
        .init();
    println!("osbox starting (config: {})", cli.config);
    Ok(())
}
