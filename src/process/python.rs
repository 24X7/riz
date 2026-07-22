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
        let handler_arg = resolve_handler_arg(&module, &export_name);

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

/// Resolve `module` (which already carries the `.py` extension, e.g. `app.py`)
/// and `export_name` into the `<abs-path-without-ext>.<export>` argument the
/// adapter expects.
///
/// Canonicalize the REAL file FIRST, then strip the extension — so the
/// AWS-canonical bare form (`app.lambda_handler`) resolves to an ABSOLUTE path.
/// The adapter treats any module path containing a separator as a file load and
/// anything else as a `sys.path` import; a bare module name would otherwise take
/// the import branch, which can't see the project dir (the adapter script's own
/// dir is what's on `sys.path`), so a stock AWS handler crash-looped with an
/// opaque "Broken pipe". Canonicalizing before stripping keeps
/// `app.lambda_handler` and `./app.lambda_handler` identical. If the file is
/// genuinely missing, still return an absolute (cwd-joined) path so the failure
/// is a clean file-load error, never a misleading "no module named".
fn resolve_handler_arg(module: &std::path::Path, export_name: &str) -> String {
    let module_abs = module.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|cwd| cwd.join(module))
            .unwrap_or_else(|_| module.to_path_buf())
    });
    format!(
        "{}.{}",
        module_abs.with_extension("").display(),
        export_name
    )
}

#[cfg(test)]
mod tests {
    use super::resolve_handler_arg;
    use std::path::{Path, MAIN_SEPARATOR};

    #[test]
    fn absolute_existing_file_resolves_to_abs_path_without_extension() {
        let dir = std::env::temp_dir().join(format!("riz-pytest-abs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.py");
        std::fs::write(&file, b"def lambda_handler(e, c): return {}\n").unwrap();

        let arg = resolve_handler_arg(&file, "lambda_handler");

        assert!(arg.contains(MAIN_SEPARATOR), "must be a path, got {arg}");
        assert!(arg.ends_with("app.lambda_handler"), "got {arg}");
        assert!(!arg.contains(".py."), "extension must be stripped: {arg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bare_relative_module_still_yields_a_path_not_a_bare_name() {
        // The bug: a bare `app.lambda_handler` used to resolve to the literal
        // string `app.lambda_handler` (no separator), which the adapter tried to
        // `import` off sys.path and failed. It must always carry a separator so
        // the adapter file-loads it. nextest runs each test in its own process,
        // so set_current_dir here is isolated.
        let dir = std::env::temp_dir().join(format!("riz-pytest-rel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.py"), b"def lambda_handler(e, c): return {}\n").unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let arg = resolve_handler_arg(Path::new("app.py"), "lambda_handler");

        assert!(
            arg.contains(MAIN_SEPARATOR),
            "bare module must resolve to a path (file-load), got bare name: {arg}"
        );
        assert!(arg.ends_with("app.lambda_handler"), "got {arg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_still_returns_a_path_not_a_bare_module() {
        // Even when the file is absent, the arg must contain a separator so the
        // adapter surfaces "cannot load file", not a misleading import error.
        let arg = resolve_handler_arg(Path::new("does-not-exist.py"), "handler");
        assert!(arg.contains(MAIN_SEPARATOR), "got {arg}");
        assert!(arg.ends_with("does-not-exist.handler"), "got {arg}");
    }
}
