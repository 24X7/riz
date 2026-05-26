use tokio::process::Command;
use crate::config::{FunctionConfig, RuntimeKind};
use crate::process::bun::BunRuntime;

pub trait LambdaRuntime: Send + Sync + 'static {
    fn spawn_command(&self, route: &FunctionConfig) -> Command;
    fn name(&self) -> &'static str;
}

pub struct RuntimeRegistry {
    bun: BunRuntime,
}

impl RuntimeRegistry {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            bun: BunRuntime::new()?,
        })
    }

    pub fn get(&self, kind: &RuntimeKind) -> &dyn LambdaRuntime {
        match kind {
            RuntimeKind::Bun => &self.bun,
            RuntimeKind::Rust | RuntimeKind::Python => {
                tracing::warn!("runtime {:?} not implemented in Phase 1 — falling back to Bun", kind);
                &self.bun
            }
        }
    }
}
