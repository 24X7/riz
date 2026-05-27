use crate::config::FunctionConfig;
use crate::process::runtime::LambdaRuntime;
use anyhow::Context;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::info;

const PYTHON_ADAPTER: &str = include_str!("../../assets/python-adapter.py");

pub struct PythonRuntime {
    /// Absolute path of the extracted adapter written to `~/.riz/python-adapter.py`.
    adapter_path: PathBuf,
    /// Resolved `python3` binary (falls back to the bare name if `which` fails).
    python_bin: String,
}

impl PythonRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let python_bin = detect_python3();
        let dir = home_dir().join(".riz");
        std::fs::create_dir_all(&dir).context("failed to create ~/.riz")?;
        let adapter_path = dir.join("python-adapter.py");
        std::fs::write(&adapter_path, PYTHON_ADAPTER)
            .context("failed to write python adapter to ~/.riz/python-adapter.py")?;
        Ok(Self {
            adapter_path,
            python_bin,
        })
    }
}

/// Detect the `python3` binary. Tries `python3` on PATH; falls back to the
/// bare string so the OS error surfaces clearly at spawn time rather than here.
fn detect_python3() -> String {
    let Ok(output) = std::process::Command::new("which").arg("python3").output() else {
        return "python3".to_owned();
    };
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() {
            return path;
        }
    }
    "python3".to_owned()
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

impl LambdaRuntime for PythonRuntime {
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // Resolve AWS-style `file.export` (e.g. `app.lambda_handler`) into a
        // concrete file path (without `.py`) and attribute name.
        // We pass them joined as a single arg: `/abs/path/to/app.lambda_handler`
        // The adapter splits on the last `.` to separate module path from attr.
        let (module, export_name) = cfg.module_and_export();

        // Strip the `.py` extension that `module_and_export` appended so the
        // adapter can re-append it when doing `spec_from_file_location`.
        let module_no_ext = module
            .with_extension("")
            .canonicalize()
            .unwrap_or_else(|_| module.with_extension(""));

        let handler_arg = format!("{}.{}", module_no_ext.display(), export_name);

        info!(
            python_bin = %self.python_bin,
            handler = %handler_arg,
            "spawning Python subprocess"
        );

        let mut cmd = Command::new(&self.python_bin);
        cmd.arg(&self.adapter_path).arg(handler_arg);
        cmd
    }

    fn name(&self) -> &'static str {
        "python"
    }
}
