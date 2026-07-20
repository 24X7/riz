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
        // Capability grants no longer ride argv. A granted worker reaches the
        // daemon broker over a UDS; its RIZ_BROKER_SOCK/TOKEN/TIMEOUT env is
        // minted per worker in the pool spawn path (src/process/pool.rs), so
        // this adapter stays credential- and grant-free.
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

/// Store data for the wasm host: the WASI context plus the capability seam.
struct HostCtx {
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    /// Sync UDS client to the daemon broker. `Some` only for a granted worker
    /// (spawned with `RIZ_BROKER_SOCK`); `None` → capability calls answer
    /// `denied` locally with zero IPC. The guest is blocked inside its own
    /// host call while this runs — no reentrancy, no async runtime needed.
    client: Option<crate::broker::client::CapabilityClient>,
    /// Response bytes from the last capability call, awaiting `read_response`.
    stash: Vec<u8>,
}

/// Parsed `riz __wasm-host` argv (everything after the subcommand).
struct HostArgs {
    module_path: String,
    dirs: Vec<String>,
    envs: Vec<(String, String)>,
}

/// Parse `<module.wasm> [--dir PATH] [--env K=V]`. Iterator-driven — no index
/// arithmetic to slip out of bounds (rule 9); every malformed shape is a
/// startup error, never a panic. (Capability grants no longer ride argv: a
/// granted worker reaches the daemon broker via `RIZ_BROKER_SOCK` env.)
fn parse_host_args(args: &[String]) -> wasmtime::Result<HostArgs> {
    use wasmtime::bail;

    let mut it = args.iter();
    let Some(module_path) = it.next() else {
        bail!("__wasm-host: missing <module.wasm> argument");
    };
    let mut dirs: Vec<String> = Vec::new();
    let mut envs: Vec<(String, String)> = Vec::new();
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
            other => bail!("__wasm-host: unexpected argument {other:?}"),
        }
    }
    Ok(HostArgs {
        module_path: module_path.clone(),
        dirs,
        envs,
    })
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
    } = parse_host_args(args)?;

    // A granted worker was spawned with RIZ_BROKER_SOCK/TOKEN; a grantless one
    // has no client and answers capability calls with `denied` locally.
    let client = crate::broker::client::CapabilityClient::from_env();

    let engine = Engine::default();
    let module = Module::from_file(&engine, &module_path)
        .with_context(|| format!("failed to load wasm module {module_path}"))?;

    let mut linker: Linker<HostCtx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |t: &mut HostCtx| &mut t.wasi)
        .context("failed to wire WASIp1 into the linker")?;
    add_capability_imports(&mut linker).context("failed to wire capability imports")?;

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
            client,
            stash: Vec::new(),
        },
    );
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(stale_abi_hint)
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

/// Sanctioned pre-1.0 ABI break (spec 2026-07-19, PR4): the v1 per-verb
/// `riz_broker.*` imports are gone. A module still importing them fails
/// closed with an actionable rebuild hint instead of a bare unknown-import
/// error; every other error passes through untouched.
fn stale_abi_hint(e: wasmtime::Error) -> wasmtime::Error {
    if format!("{e:?}").contains("riz_broker") {
        wasmtime::format_err!(
            "{e:#} — this module was built against the pre-0.2 riz-wasm \
             capability ABI; rebuild against riz-wasm >= 0.2"
        )
    } else {
        e
    }
}

/// The guest-facing capability ABI (import module `riz_capability`), v2:
///
/// - `call(verb_ptr, verb_len, grant_ptr, grant_len, req_ptr, req_len) -> i32`
///   ONE dispatcher import for every brokered verb — new capabilities are new
///   verb strings (`"pg.query"`, later `"http.fetch"`, …), never new imports.
///   Runs the brokered call; the response (success or error envelope, always
///   JSON) is stashed host-side. Returns the stashed length, or -1 for an
///   ABI-level fault (out-of-bounds pointers). With no grants armed this
///   still answers — with a `denied` envelope, never a trap. An unknown verb
///   answers the closed-set `bad_request` envelope.
/// - `read_response(dst_ptr, dst_cap) -> i32`
///   Copies the stashed response into guest memory and clears the stash;
///   returns the length. If `dst_cap` is too small, copies nothing and
///   returns the needed length (stash persists; call again with a bigger
///   buffer). 0 when nothing is stashed.
///
/// v1's per-verb `riz_broker.pg_query` import is gone (sanctioned pre-1.0
/// break): a module still importing it fails instantiation with an
/// actionable rebuild hint — see `run_host_inner`.
fn add_capability_imports(linker: &mut wasmtime::Linker<HostCtx>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "riz_capability",
        "call",
        |mut caller: wasmtime::Caller<'_, HostCtx>,
         verb_ptr: i32,
         verb_len: i32,
         grant_ptr: i32,
         grant_len: i32,
         req_ptr: i32,
         req_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };
            // Bounds-check every range against the guest's own memory size
            // before allocating host buffers — the allocation is thereby
            // proportional to memory the guest itself already paid for.
            let mem_size = memory.data_size(&caller);
            let Some((verb_ptr, verb_len)) = guest_range(verb_ptr, verb_len, mem_size) else {
                return -1;
            };
            let Some((grant_ptr, grant_len)) = guest_range(grant_ptr, grant_len, mem_size) else {
                return -1;
            };
            let Some((req_ptr, req_len)) = guest_range(req_ptr, req_len, mem_size) else {
                return -1;
            };
            let mut verb_buf = vec![0u8; verb_len];
            let mut grant_buf = vec![0u8; grant_len];
            let mut req_buf = vec![0u8; req_len];
            if memory.read(&caller, verb_ptr, &mut verb_buf).is_err()
                || memory.read(&caller, grant_ptr, &mut grant_buf).is_err()
                || memory.read(&caller, req_ptr, &mut req_buf).is_err()
            {
                return -1;
            }
            let verb = String::from_utf8_lossy(&verb_buf).into_owned();
            let grant = String::from_utf8_lossy(&grant_buf).into_owned();
            // Forward EVERY verb to the daemon broker over the UDS; verb
            // dispatch (and the unknown-verb `bad_request`) lives daemon-side
            // now. A grantless worker has no client → `denied` locally, zero
            // IPC. The call is blocking and bounded by the socket timeout, so
            // a wedged daemon returns a `timeout` envelope, never a hung guest.
            let response = match caller.data_mut().client.as_mut() {
                Some(client) => client.call(&verb, &grant, &req_buf),
                None => serde_json::json!({
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
        "riz_capability",
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

    #[test]
    fn stale_riz_broker_import_fails_with_rebuild_hint() {
        // A pre-0.2 guest importing the deleted v1 ABI must fail closed with
        // an actionable message, not a bare unknown-import linker error.
        let engine = wasmtime::Engine::default();
        let wat = r#"(module
            (import "riz_broker" "pg_query" (func (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#;
        let module = wasmtime::Module::new(&engine, wat).expect("wat parses");
        let mut linker: wasmtime::Linker<HostCtx> = wasmtime::Linker::new(&engine);
        add_capability_imports(&mut linker).expect("wire capability imports");
        let wasi = wasmtime_wasi::WasiCtxBuilder::new().build_p1();
        let mut store = wasmtime::Store::new(
            &engine,
            HostCtx {
                wasi,
                client: None,
                stash: Vec::new(),
            },
        );
        let err = linker
            .instantiate(&mut store, &module)
            .map_err(stale_abi_hint)
            .expect_err("stale import must not instantiate");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rebuild against riz-wasm >= 0.2"),
            "hint missing: {msg}"
        );
    }

    #[test]
    fn unrelated_instantiation_errors_pass_through_unhinted() {
        let e = wasmtime::format_err!("boom: something else");
        let out = format!("{:#}", stale_abi_hint(e));
        assert!(!out.contains("riz-wasm >= 0.2"));
        assert!(out.contains("boom"));
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_host_args_full_shape() {
        let args = s(&["mod.wasm", "--dir", "/data", "--env", "K=V"]);
        let parsed = parse_host_args(&args).expect("valid argv parses");
        assert_eq!(parsed.module_path, "mod.wasm");
        assert_eq!(parsed.dirs, vec!["/data".to_string()]);
        assert_eq!(parsed.envs, vec![("K".to_string(), "V".to_string())]);
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
            parse_host_args(&s(&["m.wasm", "--bogus"])).is_err(),
            "unknown flag"
        );
        assert!(
            parse_host_args(&s(&["m.wasm", "--broker-grants", "{}"])).is_err(),
            "--broker-grants is gone; grants no longer ride argv"
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
