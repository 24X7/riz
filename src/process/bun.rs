use std::path::PathBuf;
use tokio::process::Command;
use anyhow::Context;
use crate::config::RouteConfig;
use crate::process::runtime::LambdaRuntime;

const BUN_ADAPTER: &str = include_str!("../../assets/bun-adapter.mjs");

pub struct BunRuntime {
    adapter_path: PathBuf,
}

impl BunRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let dir = home_dir().join(".osbox");
        std::fs::create_dir_all(&dir)
            .context("failed to create ~/.osbox")?;
        let adapter_path = dir.join("bun-adapter.mjs");
        std::fs::write(&adapter_path, BUN_ADAPTER)
            .context("failed to write bun adapter")?;
        Ok(Self { adapter_path })
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

impl LambdaRuntime for BunRuntime {
    fn spawn_command(&self, route: &RouteConfig) -> Command {
        let handler = route.handler.canonicalize()
            .unwrap_or_else(|_| route.handler.clone());
        let mut cmd = Command::new("bun");
        cmd.arg("run")
           .arg(&self.adapter_path)
           .arg(handler);
        cmd
    }

    fn name(&self) -> &'static str { "bun" }
}
