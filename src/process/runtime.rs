use crate::config::{FunctionConfig, RuntimeKind};
use crate::process::bun::BunRuntime;
use crate::process::rust::RustRuntime;
use tokio::process::Command;

pub trait LambdaRuntime: Send + Sync + 'static {
    fn spawn_command(&self, route: &FunctionConfig) -> Command;
    fn name(&self) -> &'static str;
}

pub struct RuntimeRegistry {
    bun: BunRuntime,
    rust: RustRuntime,
}

impl RuntimeRegistry {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            bun: BunRuntime::new()?,
            rust: RustRuntime::new(),
        })
    }

    pub fn get(&self, kind: &RuntimeKind) -> &dyn LambdaRuntime {
        match kind {
            RuntimeKind::Bun => &self.bun,
            RuntimeKind::Rust => &self.rust,
            RuntimeKind::Python => {
                // Should never be reached: Config::validate rejects Python at
                // load time. Panic loudly so we never silently mis-spawn a
                // Python handler under a different runtime.
                panic!(
                    "runtime {:?} is not yet implemented. \
                     This panic indicates Config::validate did not reject the unsupported runtime.",
                    kind
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_registry_registers_bun() {
        let r = RuntimeRegistry::new().expect("registry");
        // Calling get with Bun must not panic — it proves Bun is a registered runtime.
        let _rt = r.get(&RuntimeKind::Bun);
    }
}
