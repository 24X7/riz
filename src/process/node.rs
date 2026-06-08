use crate::config::FunctionConfig;
use crate::process::runtime::LambdaRuntime;
use anyhow::Context;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::info;

const NODE_ADAPTER: &str = include_str!("../../assets/node-adapter.mjs");

pub struct NodeRuntime {
    /// Absolute path of the extracted adapter written to `~/.riz/node-adapter.mjs`.
    adapter_path: PathBuf,
    /// Resolved `node` binary (falls back to the bare name if `which` fails).
    node_bin: String,
}

impl NodeRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let node_bin = detect_node();
        let dir = home_dir().join(".riz");
        std::fs::create_dir_all(&dir).context("failed to create ~/.riz")?;
        let adapter_path = dir.join("node-adapter.mjs");
        std::fs::write(&adapter_path, NODE_ADAPTER)
            .context("failed to write node adapter to ~/.riz/node-adapter.mjs")?;
        Ok(Self {
            adapter_path,
            node_bin,
        })
    }
}

/// Detect the `node` binary. Tries `node` on PATH; falls back to the bare
/// string so the OS error surfaces clearly at spawn time rather than here.
fn detect_node() -> String {
    let Ok(output) = std::process::Command::new("which").arg("node").output() else {
        return "node".to_owned();
    };
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() {
            return path;
        }
    }
    "node".to_owned()
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

impl LambdaRuntime for NodeRuntime {
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // Resolve AWS-style `file.export` (e.g. `index.handler`) into a
        // concrete module path and export name. The adapter takes both as args.
        let (module, export_name) = cfg.module_and_export();
        let module = module.canonicalize().unwrap_or(module);

        info!(
            node_bin = %self.node_bin,
            module = %module.display(),
            export = %export_name,
            "spawning Node subprocess"
        );

        let mut cmd = Command::new(&self.node_bin);
        cmd.arg(&self.adapter_path).arg(module).arg(export_name);
        cmd
    }

    fn name(&self) -> &'static str {
        "node"
    }
}
