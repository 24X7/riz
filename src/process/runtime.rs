use crate::config::{FunctionConfig, RuntimeKind};
use crate::process::bun::BunRuntime;
use crate::process::node::NodeRuntime;
use crate::process::python::PythonRuntime;
use crate::process::rust::RustRuntime;
use tokio::process::Command;

pub trait LambdaRuntime: Send + Sync + 'static {
    fn spawn_command(&self, route: &FunctionConfig) -> Command;
    // Surfaced in runtime adapter logging and introspection endpoints.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

pub struct RuntimeRegistry {
    bun: BunRuntime,
    python: PythonRuntime,
    rust: RustRuntime,
    node: NodeRuntime,
}

impl RuntimeRegistry {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            bun: BunRuntime::new()?,
            python: PythonRuntime::new()?,
            rust: RustRuntime::new(),
            node: NodeRuntime::new()?,
        })
    }

    pub fn get(&self, kind: &RuntimeKind) -> &dyn LambdaRuntime {
        match kind {
            RuntimeKind::Bun => &self.bun,
            RuntimeKind::Python => &self.python,
            RuntimeKind::Rust => &self.rust,
            RuntimeKind::Node => &self.node,
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
}
