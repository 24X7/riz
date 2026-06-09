//! Runtime adapter + embedded host for `.wasm` Lambda handlers.
//!
//! A WASM handler is a `wasm32-wasip1` module that speaks the same
//! line-delimited JSON envelope protocol as every other runtime (read a
//! `{ event, __riz_deadline_ms, __riz_function_name }` line from stdin, write
//! a gateway-shaped response line to stdout).
//!
//! riz does **not** load the module in-process. Each pool worker is a
//! `riz __wasm-host <module.wasm> [--dir PATH] [--env K=V]` subprocess that
//! embeds wasmtime, applies the WASI capability sandbox, and runs the module's
//! `_start`. Reusing the subprocess model means the WASM runtime inherits the
//! full pool / liveness / timeout / respawn machinery for free, and the
//! capability boundary is a real OS process boundary on top of the wasm one.
//!
//! ## Capabilities (deny-by-default)
//! The guest gets stdio (inherited from the pool pipe) and nothing else by
//! default — no filesystem, no network, no host env. `allowed_paths` in
//! `riz.toml` become WASI preopens (host path == guest path); `stage_variables`
//! become guest environment variables. This is the sandbox the README's
//! "capability-sandboxed WASM" pillar refers to.

use crate::config::FunctionConfig;
use crate::process::runtime::LambdaRuntime;
use tokio::process::Command;

pub struct WasmRuntime;

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl LambdaRuntime for WasmRuntime {
    fn spawn_command(&self, cfg: &FunctionConfig) -> Command {
        // For Wasm, module_and_export() returns (module_path, "") — the
        // handler IS the `.wasm` artifact; no module/export split.
        let (module_path, _export) = cfg.module_and_export();
        // Re-invoke our own binary as the wasmtime host. `RIZ_HOST_BIN` lets a
        // wrapper (or an integration test that boots `build_app` in-process,
        // where `current_exe()` is the test runner, not riz) point at the real
        // riz binary. Otherwise current_exe() is the running riz binary;
        // falling back to "riz" on PATH is a best-effort last resort.
        let exe = std::env::var_os("RIZ_HOST_BIN")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("riz"));
        tracing::info!(
            handler = %module_path.display(),
            host = %exe.display(),
            "spawning WASM lambda under wasmtime host"
        );
        let mut cmd = Command::new(exe);
        cmd.arg("__wasm-host").arg(&module_path);
        // WASI preopens: grant each allowed path to the guest at the same
        // path. Absent → the guest has no filesystem access at all.
        if let Some(paths) = &cfg.allowed_paths {
            for p in paths {
                cmd.arg("--dir").arg(p);
            }
        }
        // stage_variables surface as guest environment variables (the WASI
        // module reads them via std::env). Host env is otherwise NOT inherited.
        for (k, v) in &cfg.stage_variables {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        cmd
    }

    fn name(&self) -> &'static str {
        "wasm"
    }
}

/// Entry point for the `riz __wasm-host <module> [--dir PATH] [--env K=V]`
/// subprocess. Synchronous on purpose: wasmtime's WASIp1 sync API drives the
/// guest's blocking stdin/stdout loop directly, so this runs *before* any tokio
/// runtime is constructed (see `main()`), keeping each wasm worker lean.
///
/// `args` is everything after `__wasm-host` (i.e. `argv[2..]`). Bridges
/// wasmtime's own error type into `anyhow` at the boundary so it slots into
/// `main()`'s `anyhow::Result`.
pub fn run_host(args: &[String]) -> anyhow::Result<()> {
    run_host_inner(args).map_err(|e| anyhow::anyhow!("{e:?}"))
}

fn run_host_inner(args: &[String]) -> wasmtime::Result<()> {
    use wasmtime::error::Context as _;
    use wasmtime::{bail, Engine, Linker, Module, Store};
    use wasmtime_wasi::p1::{self, WasiP1Ctx};
    use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

    let Some(module_path) = args.first() else {
        bail!("__wasm-host: missing <module.wasm> argument");
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut envs: Vec<(String, String)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                let Some(p) = args.get(i + 1) else {
                    bail!("__wasm-host: --dir requires a path argument");
                };
                dirs.push(p.clone());
                i += 2;
            }
            "--env" => {
                let Some(kv) = args.get(i + 1) else {
                    bail!("__wasm-host: --env requires a KEY=VALUE argument");
                };
                if let Some((k, v)) = kv.split_once('=') {
                    envs.push((k.to_string(), v.to_string()));
                }
                i += 2;
            }
            other => bail!("__wasm-host: unexpected argument {other:?}"),
        }
    }

    let engine = Engine::default();
    let module = Module::from_file(&engine, module_path)
        .with_context(|| format!("failed to load wasm module {module_path}"))?;

    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |t| t)
        .context("failed to wire WASIp1 into the linker")?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdin().inherit_stdout().inherit_stderr();
    for d in &dirs {
        builder
            .preopened_dir(d, d, DirPerms::all(), FilePerms::all())
            .with_context(|| format!("failed to preopen --dir {d}"))?;
    }
    for (k, v) in &envs {
        builder.env(k, v);
    }
    let wasi = builder.build_p1();

    let mut store = Store::new(&engine, wasi);
    let instance = linker
        .instantiate(&mut store, &module)
        .context("failed to instantiate wasm module")?;
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .context("wasm module has no `_start` export (not a wasip1 command?)")?;

    match start.call(&mut store, ()) {
        Ok(()) => Ok(()),
        Err(e) => {
            // A clean `proc_exit(0)` (e.g. main() returning on stdin EOF) is
            // surfaced as an I32Exit trap — code 0 is success, not an error.
            if let Some(exit) = e.downcast_ref::<I32Exit>() {
                if exit.0 == 0 {
                    return Ok(());
                }
                bail!("wasm guest exited with code {}", exit.0);
            }
            Err(e).context("wasm guest trapped")
        }
    }
}
