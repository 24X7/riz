use crate::config::FunctionConfig;
use crate::process::runtime::{LambdaRuntime, WorkerTransport};
use tokio::process::Command;

/// Runtime adapter for **pre-compiled native binaries** that are UNMODIFIED
/// official AWS Lambda runtime clients (Go `aws-lambda-go`, Rust
/// `lambda_runtime`, any `provided.al2023`). Used by `runtime = "rust"` and
/// `runtime = "go"`: there is no riz library and no code change — the binary
/// speaks the real **AWS Lambda Runtime API**, which riz serves per worker
/// (see [`crate::process::runtime_api`]). This adapter just `exec`s the binary;
/// the pool provisions the Runtime-API endpoint and sets `AWS_LAMBDA_RUNTIME_API`
/// (the `RuntimeApi` transport). The only per-language difference is the `name`
/// reported to logs and `/_riz/health`.
pub struct StaticBinaryRuntime {
    name: &'static str,
}

impl StaticBinaryRuntime {
    pub fn new(name: &'static str) -> Self {
        Self { name }
    }
}

impl LambdaRuntime for StaticBinaryRuntime {
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // For native binaries, module_and_export() returns (handler_path, "")
        // — the handler IS the executable; no module/export split is meaningful.
        let (binary_path, _export) = cfg.module_and_export();
        tracing::info!(
            runtime = self.name,
            handler = %binary_path.display(),
            "spawning native lambda binary"
        );
        Command::new(&binary_path)
    }

    fn name(&self) -> &'static str {
        self.name
    }

    /// Native binaries are unmodified official AWS Lambda runtime clients — they
    /// speak the Lambda Runtime API over `AWS_LAMBDA_RUNTIME_API`, not stdio.
    fn transport(&self) -> WorkerTransport {
        WorkerTransport::RuntimeApi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FunctionConfig, RuntimeKind};

    fn fc(runtime: RuntimeKind, handler: &str) -> FunctionConfig {
        let toml = format!(
            "runtime = {:?}\nhandler = {:?}\n",
            runtime.as_str(),
            handler
        );
        toml::from_str(&toml).expect("function config parses")
    }

    #[test]
    fn execs_the_handler_binary_path_verbatim() {
        let rt = StaticBinaryRuntime::new("go");
        let cfg = fc(RuntimeKind::Go, "./bin/my-go-app");
        let cmd = rt.spawn_command(&cfg);
        assert_eq!(
            cmd.as_std().get_program().to_string_lossy(),
            "./bin/my-go-app"
        );
        assert_eq!(rt.name(), "go");
    }

    #[test]
    fn rust_and_go_share_the_same_spawner() {
        assert_eq!(StaticBinaryRuntime::new("rust").name(), "rust");
        assert_eq!(StaticBinaryRuntime::new("go").name(), "go");
    }
}
