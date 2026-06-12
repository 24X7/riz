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
        // Broker capability grants (resource broker v1). The grants are limit
        // config only — resource definitions ride the RIZ_BROKER_RESOURCES
        // env var (set at startup) and DSNs resolve from the child's own
        // inherited environment. No credential ever appears in argv.
        if !cfg.capabilities.is_empty() {
            if let Ok(json) = serde_json::to_string(&cfg.capabilities) {
                cmd.arg("--broker-grants").arg(json);
            }
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

/// Store data for the wasm host: the WASI context plus the broker seam.
struct HostCtx {
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    /// Armed when the function has `[function.x.capabilities]` grants.
    broker: Option<std::sync::Arc<crate::broker::Broker>>,
    /// Current-thread tokio runtime driving the broker's async I/O. The
    /// guest is blocked inside its own host call while this runs — no
    /// reentrancy.
    rt: Option<std::sync::Arc<tokio::runtime::Runtime>>,
    /// Response bytes from the last broker call, awaiting `read_response`.
    stash: Vec<u8>,
}

fn run_host_inner(args: &[String]) -> wasmtime::Result<()> {
    use wasmtime::error::Context as _;
    use wasmtime::{bail, Engine, Linker, Module, Store};
    use wasmtime_wasi::p1;
    use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

    let Some(module_path) = args.first() else {
        bail!("__wasm-host: missing <module.wasm> argument");
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut envs: Vec<(String, String)> = Vec::new();
    let mut broker_grants_json: Option<String> = None;
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
            "--broker-grants" => {
                let Some(json) = args.get(i + 1) else {
                    bail!("__wasm-host: --broker-grants requires a JSON argument");
                };
                broker_grants_json = Some(json.clone());
                i += 2;
            }
            other => bail!("__wasm-host: unexpected argument {other:?}"),
        }
    }

    // ── Resource broker (capability grants) ─────────────────────────────
    // Grants arrive in argv (limit config only); [resources] definitions ride
    // RIZ_BROKER_RESOURCES in the inherited env; DSNs resolve from the env
    // vars the resources name. Failures here are startup errors — a granted
    // function that can't arm its broker must not come up half-armed.
    let (broker, rt) = match broker_grants_json {
        None => (None, None),
        Some(json) => {
            let grants: indexmap::IndexMap<String, crate::config::CapabilityGrant> =
                serde_json::from_str(&json).context("--broker-grants is not valid JSON")?;
            let resources_json = std::env::var("RIZ_BROKER_RESOURCES")
                .context("--broker-grants given but RIZ_BROKER_RESOURCES is not set")?;
            let resources: crate::config::ResourcesConfig =
                serde_json::from_str(&resources_json)
                    .context("RIZ_BROKER_RESOURCES is not valid JSON")?;
            let backends = crate::broker::pg::backends_for_function(&grants, &resources)
                .map_err(|e| wasmtime::format_err!("broker backend setup failed: {e}"))?;
            let broker = std::sync::Arc::new(crate::broker::Broker::new(&grants, backends));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build broker runtime")?;
            (Some(broker), Some(std::sync::Arc::new(rt)))
        }
    };

    let engine = Engine::default();
    let module = Module::from_file(&engine, module_path)
        .with_context(|| format!("failed to load wasm module {module_path}"))?;

    let mut linker: Linker<HostCtx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |t: &mut HostCtx| &mut t.wasi)
        .context("failed to wire WASIp1 into the linker")?;
    add_broker_imports(&mut linker).context("failed to wire broker imports")?;

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

    let mut store = Store::new(
        &engine,
        HostCtx {
            wasi,
            broker,
            rt,
            stash: Vec::new(),
        },
    );
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

/// The guest-facing broker ABI (import module `riz_broker`), v1:
///
/// - `pg_query(grant_ptr, grant_len, req_ptr, req_len) -> i32`
///   Runs the brokered call; the response (success or error envelope, always
///   JSON) is stashed host-side. Returns the stashed length, or -1 for an
///   ABI-level fault (out-of-bounds pointers). With no grants armed this
///   still answers — with a `denied` envelope, never a trap.
/// - `read_response(dst_ptr, dst_cap) -> i32`
///   Copies the stashed response into guest memory and clears the stash;
///   returns the length. If `dst_cap` is too small, copies nothing and
///   returns the needed length (stash persists; call again with a bigger
///   buffer). 0 when nothing is stashed.
fn add_broker_imports(linker: &mut wasmtime::Linker<HostCtx>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "riz_broker",
        "pg_query",
        |mut caller: wasmtime::Caller<'_, HostCtx>,
         grant_ptr: i32,
         grant_len: i32,
         req_ptr: i32,
         req_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };
            let mut grant_buf = vec![0u8; grant_len.max(0) as usize];
            let mut req_buf = vec![0u8; req_len.max(0) as usize];
            if memory.read(&caller, grant_ptr as usize, &mut grant_buf).is_err()
                || memory.read(&caller, req_ptr as usize, &mut req_buf).is_err()
            {
                return -1;
            }
            let grant = String::from_utf8_lossy(&grant_buf).into_owned();
            let (broker, rt) = {
                let ctx = caller.data();
                (ctx.broker.clone(), ctx.rt.clone())
            };
            let response = match (broker, rt) {
                (Some(broker), Some(rt)) => rt.block_on(broker.pg_query(&grant, &req_buf)),
                // No grants armed: deny-by-default, as an envelope the guest
                // can parse — never a trap.
                _ => serde_json::json!({
                    "ok": false,
                    "error": {"code": "denied", "message": "function has no capability grants"}
                })
                .to_string()
                .into_bytes(),
            };
            let len = response.len() as i32;
            caller.data_mut().stash = response;
            len
        },
    )?;
    linker.func_wrap(
        "riz_broker",
        "read_response",
        |mut caller: wasmtime::Caller<'_, HostCtx>, dst_ptr: i32, dst_cap: i32| -> i32 {
            let stash_len = caller.data().stash.len() as i32;
            if stash_len == 0 {
                return 0;
            }
            if dst_cap < stash_len {
                return stash_len; // tell the guest how much room it needs
            }
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };
            let stash = std::mem::take(&mut caller.data_mut().stash);
            if memory.write(&mut caller, dst_ptr as usize, &stash).is_err() {
                // Restore so the guest can retry with a valid pointer.
                caller.data_mut().stash = stash;
                return -1;
            }
            stash_len
        },
    )?;
    Ok(())
}
