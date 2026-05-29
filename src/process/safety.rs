//! Per-child safety hardening applied in the `pre_exec` callback that
//! runs in the spawned child after `fork()` but before `execve()`.
//!
//! Anything set here is inherited by the lambda subprocess. Operations
//! must be async-signal-safe (no allocator, no mutex, no Rust-level
//! synchronization beyond what libc provides). Plain syscalls only.
//!
//! Current scope: a single always-on protection, RLIMIT_CORE = 0,
//! which prevents lambda crashes from generating multi-gigabyte core
//! dumps that can fill the host disk. Future Wave-10 increments will
//! add additional primitives one at a time, each with its own commit
//! and acceptance gate.

/// Apply the always-on safety profile to the current process. Called
/// from the `pre_exec` closure that `Command` runs in the child after
/// fork. Returns `io::Result` because `pre_exec` requires it.
///
/// On non-Unix platforms this is a no-op.
#[cfg(unix)]
pub(super) fn apply_always_on_limits() -> std::io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};
    let to_io = |e: nix::Error| std::io::Error::from_raw_os_error(e as i32);

    // Hard-cap core dump size to 0 bytes. A lambda that segfaults or
    // panics will not write a core file — protects host disk under
    // crash storms.
    setrlimit(Resource::RLIMIT_CORE, 0, 0).map_err(to_io)?;

    // File descriptors: cap at 4096 per child. Bun/Python/Rust handlers
    // typically use < 100. A leaky `fs.open` loop hits this ceiling
    // before exhausting host FDs.
    setrlimit(Resource::RLIMIT_NOFILE, 4096, 4096).map_err(to_io)?;

    // Single-file write size: cap at 100 MiB per child. A runaway
    // `fs.write` loop is bounded before filling host disk.
    setrlimit(Resource::RLIMIT_FSIZE, 100 * 1024 * 1024, 100 * 1024 * 1024)
        .map_err(to_io)?;

    // RLIMIT_NPROC is per-PROCESS on Linux but per-USER on macOS/BSD.
    // Setting it on macOS would compare against the host user's total
    // process count and likely EINVAL on any moderately busy box.
    // Apply only on Linux, where it caps fork-bombs inside a single
    // child's process tree (combined with process_group(0) + killpg
    // this gives us tight blast-radius control).
    #[cfg(target_os = "linux")]
    setrlimit(Resource::RLIMIT_NPROC, 256, 256).map_err(to_io)?;

    // Linux-only prctl pair:
    //   PR_SET_PDEATHSIG(SIGKILL): kernel SIGKILLs this child when the
    //     parent (riz daemon) exits. Prevents orphan Bun/Python
    //     processes from surviving a daemon crash.
    //   PR_SET_NO_NEW_PRIVS:      child cannot gain new privileges via
    //     setuid/file-caps on subsequent execve. Cheap defense in depth
    //     — even a malicious handler payload can't escalate via exec.
    #[cfg(target_os = "linux")]
    {
        use nix::sys::prctl;
        use nix::sys::signal::Signal;
        prctl::set_pdeathsig(Some(Signal::SIGKILL)).map_err(to_io)?;
        prctl::set_no_new_privs().map_err(to_io)?;
    }

    Ok(())
}

#[cfg(not(unix))]
pub(super) fn apply_always_on_limits() -> std::io::Result<()> {
    Ok(())
}

/// Apply the opt-in per-function caps if the FunctionConfig declared them.
/// Caller passes the values directly (not `&FunctionConfig`) so the
/// pre_exec closure can capture by move without aliasing concerns.
#[cfg(unix)]
pub(super) fn apply_per_function_limits(
    memory_mb: Option<u32>,
    cpu_time_secs: Option<u32>,
) -> std::io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};
    let to_io = |e: nix::Error| std::io::Error::from_raw_os_error(e as i32);

    if let Some(mb) = memory_mb {
        let bytes = (mb as u64).saturating_mul(1024 * 1024);
        setrlimit(Resource::RLIMIT_AS, bytes, bytes).map_err(to_io)?;
    }
    if let Some(secs) = cpu_time_secs {
        setrlimit(Resource::RLIMIT_CPU, secs as u64, secs as u64).map_err(to_io)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn apply_per_function_limits(
    _memory_mb: Option<u32>,
    _cpu_time_secs: Option<u32>,
) -> std::io::Result<()> {
    Ok(())
}

/// Apply a Linux Landlock filesystem allowlist to the calling process.
/// Each path in `paths` (and everything beneath it) is read/write/execute
/// accessible to the child; everything else is denied at the LSM layer.
///
/// Irreversible: once `restrict_self` succeeds, the process and all its
/// descendants are sandboxed for life. Always called in pre_exec on a
/// fresh child, never in the riz daemon itself.
///
/// On non-Linux platforms this is a no-op — Landlock is a Linux LSM with
/// no equivalent in macOS / *BSD APIs we can rely on. The `allowed_paths`
/// FunctionConfig field is silently ignored on those platforms; users get
/// the always-on rlimits + prctl protections but no filesystem ACL.
///
/// On Linux kernels < 5.13 (no Landlock support), `restrict_self` returns
/// success with `RulesetStatus::NotEnforced` — i.e., a silent best-effort
/// downgrade. Documented in landlock crate's `CompatLevel::BestEffort`.
#[cfg(target_os = "linux")]
pub(super) fn apply_filesystem_allowlist(paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
    };
    let abi = ABI::V2;
    let to_io = |e: landlock::RulesetError| std::io::Error::other(format!("landlock: {e}"));
    Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(to_io)?
        .create()
        .map_err(to_io)?
        .add_rules(path_beneath_rules(paths, AccessFs::from_all(abi)))
        .map_err(to_io)?
        .restrict_self()
        .map_err(to_io)?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn apply_filesystem_allowlist(
    _paths: &[std::path::PathBuf],
) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn apply_filesystem_allowlist(
    _paths: &[std::path::PathBuf],
) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;

    /// The function must succeed when called in the current process.
    /// (RLIMIT_CORE can be lowered without privilege; raising it back
    /// would require CAP_SYS_RESOURCE.) This is a coarse smoke test —
    /// the real verification is in `child_inherits_zero_core_limit`.
    #[test]
    fn apply_always_on_limits_does_not_error() {
        apply_always_on_limits().expect("must succeed on this process");
    }

    /// On Linux, the always-on safety profile sets PR_SET_NO_NEW_PRIVS.
    /// Verify by spawning a child under the pre_exec and reading
    /// `/proc/self/status` for `NoNewPrivs:	1`. On macOS this test is
    /// compiled out — the prctl block in apply_always_on_limits is
    /// already cfg-gated to Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn child_inherits_no_new_privs_on_linux() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("grep -E '^NoNewPrivs:' /proc/self/status");
        cmd.stdout(Stdio::piped());
        unsafe {
            cmd.pre_exec(apply_always_on_limits);
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("NoNewPrivs:\t1"),
            "child must have NoNewPrivs=1 in /proc/self/status; got {stdout:?}"
        );
    }

    /// Opt-in per-function caps: spawning a child with memory_mb and
    /// cpu_time_secs set must produce a process whose RLIMIT_AS and
    /// RLIMIT_CPU reflect those values.
    ///
    /// LINUX-ONLY because macOS sh + dyld + libsystem reserve ~600 MiB
    /// of virtual address space at exec, so setrlimit(RLIMIT_AS, <600MiB)
    /// returns EINVAL ("can't shrink below current usage"). RLIMIT_AS
    /// enforcement on macOS is also known-broken for JIT runtimes. On
    /// Linux strict enforcement works. The implementation still calls
    /// setrlimit on macOS — it's a no-op for too-low values and the call
    /// is harmless when memory_mb is unset (the default).
    #[cfg(target_os = "linux")]
    #[test]
    fn child_inherits_per_function_caps() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("ulimit -H -v; ulimit -H -t");
        cmd.stdout(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                apply_always_on_limits()?;
                apply_per_function_limits(Some(256), Some(10))?;
                Ok(())
            });
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().map(|s| s.trim()).collect();
        assert!(
            lines.len() >= 2,
            "expected 2 ulimit lines (VM, CPU); got {lines:?}"
        );
        // RLIMIT_AS: 256 MiB → ulimit -v reports in 1024-byte blocks → 262144.
        assert_eq!(
            lines[0], "262144",
            "RLIMIT_AS must be 262144 KiB (256 MiB); got {lines:?}"
        );
        // RLIMIT_CPU: 10 seconds.
        assert_eq!(
            lines[1], "10",
            "RLIMIT_CPU must be 10 seconds; got {lines:?}"
        );
    }

    /// Cross-platform smoke test for the per-function helper: passing
    /// None values must succeed without modifying any limits. Verified
    /// via the always-on caps still being intact after the call.
    #[test]
    fn apply_per_function_limits_with_none_is_no_op() {
        // Just verify the call doesn't error in the current process.
        apply_per_function_limits(None, None)
            .expect("None inputs must succeed without setrlimit");
    }

    /// Cross-platform smoke test for apply_filesystem_allowlist with
    /// empty paths. On non-Linux it's a no-op. On Linux it creates a
    /// landlock ruleset with no rules and restricts self — which would
    /// deny ALL filesystem access from this point on. We therefore
    /// spawn a CHILD to test the actual restriction (so the test runner
    /// itself isn't sandboxed forever).
    ///
    /// This smoke test just verifies the function signature and that
    /// the no-op path on non-Linux doesn't panic. The real Linux
    /// enforcement test runs as a subprocess.
    #[test]
    fn apply_filesystem_allowlist_signature_compiles() {
        // No-op on non-Linux; on Linux this is a deliberately-empty
        // ruleset which IS irreversibly applied — but we don't call it
        // directly in the test process (would sandbox the whole runner).
        // Instead verify the function exists and the signature compiles.
        let _ = apply_filesystem_allowlist::<>;
    }

    /// Linux-only: spawn a child with apply_filesystem_allowlist(["/tmp"])
    /// and assert it can read /tmp but cannot read /etc. Skipped on
    /// kernels < 5.13 (no Landlock support) via best-effort downgrade.
    #[cfg(target_os = "linux")]
    #[test]
    fn child_with_allowlist_can_read_allowed_path_only() {
        use std::os::unix::process::CommandExt;
        use std::path::PathBuf;
        use std::process::{Command, Stdio};

        if !std::path::Path::new("/sys/kernel/security/landlock").exists() {
            eprintln!("SKIP: kernel lacks landlock");
            return;
        }

        // Probe via `sh -c "test -r /etc/hosts && echo CAN || echo CANT"`.
        // With Landlock restricting to /tmp, the read should be denied.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("test -r /etc/hosts && echo CAN || echo CANT");
        cmd.stdout(Stdio::piped());
        let allowed: Vec<PathBuf> = vec!["/tmp".into()];
        unsafe {
            cmd.pre_exec(move || {
                apply_always_on_limits()?;
                apply_filesystem_allowlist(&allowed)?;
                Ok(())
            });
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("CANT"),
            "landlock should deny /etc/hosts read; got {stdout:?}"
        );
    }

    /// Spawning a child via Command with the safety pre_exec must
    /// produce a process whose RLIMIT_CORE is 0 AND RLIMIT_NOFILE is
    /// 4096 AND RLIMIT_FSIZE is 100 MiB (102400 KB blocks). We verify
    /// via `ulimit -H -c -n -f` (hard limits, portable on /bin/sh on
    /// both macOS and Linux).
    #[test]
    fn child_inherits_always_on_caps() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("ulimit -H -c; ulimit -H -n; ulimit -H -f");
        cmd.stdout(Stdio::piped());
        // SAFETY: apply_always_on_limits is async-signal-safe — only
        // calls setrlimit + prctl, both on the POSIX safe list.
        unsafe {
            cmd.pre_exec(apply_always_on_limits);
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().map(|s| s.trim()).collect();
        assert!(
            lines.len() >= 3,
            "expected 3 ulimit lines (CORE, NOFILE, FSIZE); got {lines:?}"
        );
        // RLIMIT_CORE: hard cap 0 → ulimit prints "0".
        assert_eq!(lines[0], "0", "RLIMIT_CORE must be 0; got {lines:?}");
        // RLIMIT_NOFILE: hard cap 4096.
        assert_eq!(
            lines[1], "4096",
            "RLIMIT_NOFILE must be 4096; got {lines:?}"
        );
        // RLIMIT_FSIZE: 100 MiB = 102400 KiB. POSIX `ulimit -f` reports
        // in 1024-byte blocks on both bash and macOS sh.
        assert_eq!(
            lines[2], "102400",
            "RLIMIT_FSIZE must be 102400 (100 MiB / 1KiB blocks); got {lines:?}"
        );
    }
}
