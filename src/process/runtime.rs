use crate::config::{FunctionConfig, RuntimeKind};
use crate::process::bun::BunRuntime;
use crate::process::node::NodeRuntime;
use crate::process::python::PythonRuntime;
use crate::process::static_binary::StaticBinaryRuntime;
use crate::process::wasm::WasmRuntime;
use tokio::process::Command;

/// How riz talks to a worker process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerTransport {
    /// riz's line-JSON envelope over the child's stdin/stdout (bun/node/python:
    /// riz's embedded adapter calls the user's exported handler).
    Stdio,
    /// The real **AWS Lambda Runtime API** over HTTP. The child is an UNMODIFIED
    /// official runtime client (Go `aws-lambda-go`, Rust `lambda_runtime`, any
    /// `provided.al2023`) that polls `AWS_LAMBDA_RUNTIME_API`. No riz library.
    RuntimeApi,
}

pub trait LambdaRuntime: Send + Sync + 'static {
    fn spawn_command(&self, route: &FunctionConfig) -> Command;
    // Surfaced in runtime adapter logging and introspection endpoints.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    /// Transport for this runtime. Defaults to stdio (scripted adapters);
    /// compiled native binaries override to the AWS Runtime API.
    fn transport(&self) -> WorkerTransport {
        WorkerTransport::Stdio
    }
}

pub struct RuntimeRegistry {
    bun: BunRuntime,
    python: PythonRuntime,
    rust: StaticBinaryRuntime,
    go: StaticBinaryRuntime,
    node: NodeRuntime,
    wasm: WasmRuntime,
}

impl RuntimeRegistry {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            bun: BunRuntime::new()?,
            python: PythonRuntime::new()?,
            // Rust and Go are both pre-compiled native binaries — same spawner,
            // different name. See `static_binary::StaticBinaryRuntime`.
            rust: StaticBinaryRuntime::new("rust"),
            go: StaticBinaryRuntime::new("go"),
            node: NodeRuntime::new()?,
            wasm: WasmRuntime::new(),
        })
    }

    pub fn get(&self, kind: &RuntimeKind) -> &dyn LambdaRuntime {
        match kind {
            RuntimeKind::Bun => &self.bun,
            RuntimeKind::Python => &self.python,
            RuntimeKind::Rust => &self.rust,
            RuntimeKind::Go => &self.go,
            RuntimeKind::Node => &self.node,
            RuntimeKind::Wasm => &self.wasm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_registry_registers_bun() {
        let r = RuntimeRegistry::new().expect("registry");
        let rt = r.get(&RuntimeKind::Bun);
        assert_eq!(rt.name(), "bun");
    }

    #[test]
    fn runtime_registry_registers_python() {
        let r = RuntimeRegistry::new().expect("registry");
        let rt = r.get(&RuntimeKind::Python);
        assert_eq!(rt.name(), "python");
    }

    #[test]
    fn runtime_registry_registers_rust() {
        let r = RuntimeRegistry::new().expect("registry");
        let rt = r.get(&RuntimeKind::Rust);
        assert_eq!(rt.name(), "rust");
    }

    #[test]
    fn runtime_registry_registers_node() {
        let r = RuntimeRegistry::new().expect("registry");
        let rt = r.get(&RuntimeKind::Node);
        assert_eq!(rt.name(), "node");
    }

    #[test]
    fn runtime_registry_registers_wasm() {
        let r = RuntimeRegistry::new().expect("registry");
        let rt = r.get(&RuntimeKind::Wasm);
        assert_eq!(rt.name(), "wasm");
    }
}
