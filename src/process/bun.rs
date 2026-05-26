use std::path::PathBuf;
use tokio::process::Command;
use anyhow::Context;
use crate::config::FunctionConfig;
use crate::process::runtime::LambdaRuntime;

const BUN_ADAPTER: &str = include_str!("../../assets/bun-adapter.mjs");

pub struct BunRuntime {
    adapter_path: PathBuf,
}

impl BunRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let dir = home_dir().join(".riz");
        std::fs::create_dir_all(&dir)
            .context("failed to create ~/.riz")?;
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
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // Resolve AWS-style `file.export` (e.g. `index.handler`) into a
        // concrete module path and export name. The adapter takes both as args.
        let (module, export_name) = cfg.module_and_export();
        let module = module.canonicalize().unwrap_or(module);
        let mut cmd = Command::new("bun");
        cmd.arg("run")
           .arg(&self.adapter_path)
           .arg(module)
           .arg(export_name);
        cmd
    }

    fn name(&self) -> &'static str { "bun" }
}
