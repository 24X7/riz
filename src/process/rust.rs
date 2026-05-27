use crate::config::FunctionConfig;
use crate::process::runtime::LambdaRuntime;
use tokio::process::Command;

/// Runtime adapter for pre-compiled Rust Lambda binaries.
///
/// Unlike the Bun adapter, there is no intermediate adapter script: the
/// user's binary IS the adapter. The `riz-rust-runtime` helper crate
/// compiled into the binary handles the line-JSON envelope loop. Riz simply
/// `exec`s the binary path from `handler` with stdin/stdout piped.
pub struct RustRuntime;

impl Default for RustRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl RustRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl LambdaRuntime for RustRuntime {
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // For Rust, module_and_export() returns (handler_path, "") — the
        // handler IS the executable; no module/export split is meaningful.
        let (binary_path, _export) = cfg.module_and_export();
        tracing::info!(
            handler = %binary_path.display(),
            "spawning Rust lambda binary"
        );
        Command::new(&binary_path)
    }

    fn name(&self) -> &'static str {
        "rust"
    }
}
