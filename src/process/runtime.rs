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
                // Should never be reached: Config::validate rejects unsupported
                // runtimes at load time. Panic loudly so we never silently run
                // a Python handler under Bun (which would simply fail to parse
                // the `.py` file as a JS module).
                panic!(
                    "runtime {:?} is not yet implemented. Riz currently supports only `bun`. \
                     This panic indicates Config::validate did not reject the unsupported runtime.",
                    kind
                );
            }
        }
    }
}
