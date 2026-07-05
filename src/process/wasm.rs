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

/// Parsed `riz __wasm-host` argv (everything after the subcommand).
struct HostArgs {
    module_path: String,
    dirs: Vec<String>,
    envs: Vec<(String, String)>,
    broker_grants_json: Option<String>,
}

/// Parse `<module.wasm> [--dir PATH] [--env K=V] [--broker-grants JSON]`.
/// Iterator-driven — no index arithmetic to slip out of bounds (rule 9);
/// every malformed shape is a startup error, never a panic.
fn parse_host_args(args: &[String]) -> wasmtime::Result<HostArgs> {
    use wasmtime::bail;

    let mut it = args.iter();
    let Some(module_path) = it.next() else {
        bail!("__wasm-host: missing <module.wasm> argument");
    };
    let mut dirs: Vec<String> = Vec::new();
    let mut envs: Vec<(String, String)> = Vec::new();
    let mut broker_grants_json: Option<String> = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dir" => {
                let Some(p) = it.next() else {
                    bail!("__wasm-host: --dir requires a path argument");
                };
                dirs.push(p.clone());
            }
            "--env" => {
                let Some(kv) = it.next() else {
                    bail!("__wasm-host: --env requires a KEY=VALUE argument");
                };
                if let Some((k, v)) = kv.split_once('=') {
                    envs.push((k.to_string(), v.to_string()));
                }
            }
            "--broker-grants" => {
                let Some(json) = it.next() else {
                    bail!("__wasm-host: --broker-grants requires a JSON argument");
                };
                broker_grants_json = Some(json.clone());
            }
            other => bail!("__wasm-host: unexpected argument {other:?}"),
        }
    }
    Ok(HostArgs {
        module_path: module_path.clone(),
        dirs,
        envs,
        broker_grants_json,
    })
}

/// A broker armed with the current-thread runtime that drives its async I/O
/// (both `None` when the function has no capability grants).
type ArmedBroker = (
    Option<std::sync::Arc<crate::broker::Broker>>,
    Option<std::sync::Arc<tokio::runtime::Runtime>>,
);

/// Arm the resource broker from `--broker-grants` + `RIZ_BROKER_RESOURCES`.
///
/// Grants arrive in argv (limit config only); `[resources]` definitions ride
/// RIZ_BROKER_RESOURCES in the inherited env; DSNs resolve from the env
/// vars the resources name. Failures here are startup errors — a granted
/// function that can't arm its broker must not come up half-armed.
fn arm_broker(broker_grants_json: Option<String>) -> wasmtime::Result<ArmedBroker> {
    use wasmtime::error::Context as _;

    let Some(json) = broker_grants_json else {
        return Ok((None, None));
    };
    let grants: indexmap::IndexMap<String, crate::config::CapabilityGrant> =
        serde_json::from_str(&json).context("--broker-grants is not valid JSON")?;
    let resources_json = std::env::var("RIZ_BROKER_RESOURCES")
        .context("--broker-grants given but RIZ_BROKER_RESOURCES is not set")?;
    let resources: crate::config::ResourcesConfig =
        serde_json::from_str(&resources_json).context("RIZ_BROKER_RESOURCES is not valid JSON")?;
    let backends = crate::broker::pg::backends_for_function(&grants, &resources)
        .map_err(|e| wasmtime::format_err!("broker backend setup failed: {e}"))?;
    let broker = std::sync::Arc::new(crate::broker::Broker::new(&grants, backends));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build broker runtime")?;
    Ok((Some(broker), Some(std::sync::Arc::new(rt))))
}

fn run_host_inner(args: &[String]) -> wasmtime::Result<()> {
    use wasmtime::error::Context as _;
    use wasmtime::{bail, Engine, Linker, Module, Store};
    use wasmtime_wasi::p1;
    use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

    let HostArgs {
        module_path,
        dirs,
        envs,
        broker_grants_json,
    } = parse_host_args(args)?;

    let (broker, rt) = arm_broker(broker_grants_json)?;

    let engine = Engine::default();
    let module = Module::from_file(&engine, &module_path)
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

/// Validate a guest-supplied `(ptr, len)` pair against the guest's linear
/// memory size BEFORE any host-side allocation. A hostile guest passing
/// `len = i32::MAX` must fault the ABI call (−1), not make the host allocate
/// gigabytes (rule 3: no unbounded growth from guest input). Returns the
/// in-bounds pair as `usize`s.
fn guest_range(ptr: i32, len: i32, mem_size: usize) -> Option<(usize, usize)> {
    let ptr = usize::try_from(ptr).ok()?; // negative → fault
    let len = usize::try_from(len).ok()?; // negative → fault
    if ptr.checked_add(len)? > mem_size {
        return None;
    }
    Some((ptr, len))
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
            // Bounds-check both ranges against the guest's own memory size
            // before allocating host buffers — the allocation is thereby
            // proportional to memory the guest itself already paid for.
            let mem_size = memory.data_size(&caller);
            let Some((grant_ptr, grant_len)) = guest_range(grant_ptr, grant_len, mem_size) else {
                return -1;
            };
            let Some((req_ptr, req_len)) = guest_range(req_ptr, req_len, mem_size) else {
                return -1;
            };
            let mut grant_buf = vec![0u8; grant_len];
            let mut req_buf = vec![0u8; req_len];
            if memory.read(&caller, grant_ptr, &mut grant_buf).is_err()
                || memory.read(&caller, req_ptr, &mut req_buf).is_err()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_host_args_full_shape() {
        let args = s(&[
            "mod.wasm",
            "--dir",
            "/data",
            "--env",
            "K=V",
            "--broker-grants",
            "{}",
        ]);
        let parsed = parse_host_args(&args).expect("valid argv parses");
        assert_eq!(parsed.module_path, "mod.wasm");
        assert_eq!(parsed.dirs, vec!["/data".to_string()]);
        assert_eq!(parsed.envs, vec![("K".to_string(), "V".to_string())]);
        assert_eq!(parsed.broker_grants_json.as_deref(), Some("{}"));
    }

    #[test]
    fn parse_host_args_rejects_malformed_shapes() {
        assert!(parse_host_args(&[]).is_err(), "missing module");
        assert!(
            parse_host_args(&s(&["m.wasm", "--dir"])).is_err(),
            "dangling --dir"
        );
        assert!(
            parse_host_args(&s(&["m.wasm", "--env"])).is_err(),
            "dangling --env"
        );
        assert!(
            parse_host_args(&s(&["m.wasm", "--broker-grants"])).is_err(),
            "dangling --broker-grants"
        );
        assert!(
            parse_host_args(&s(&["m.wasm", "--bogus"])).is_err(),
            "unknown flag"
        );
    }

    /// Rule 3: a hostile (ptr, len) from the guest must fault WITHOUT any
    /// host-side allocation — including `len = i32::MAX` and negative values.
    #[test]
    fn guest_range_rejects_hostile_pairs_before_allocating() {
        let mem = 64 * 1024; // one wasm page
        assert_eq!(guest_range(0, 16, mem), Some((0, 16)));
        assert_eq!(guest_range(0, 0, mem), Some((0, 0)));
        assert_eq!(guest_range(mem as i32 - 4, 4, mem), Some((mem - 4, 4)));
        assert_eq!(guest_range(mem as i32 - 4, 5, mem), None, "spills past end");
        assert_eq!(guest_range(0, i32::MAX, mem), None, "giant len faults");
        assert_eq!(guest_range(i32::MAX, i32::MAX, mem), None, "no usize wrap");
        assert_eq!(guest_range(-1, 4, mem), None, "negative ptr faults");
        assert_eq!(guest_range(0, -1, mem), None, "negative len faults");
    }
}
