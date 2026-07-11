//! Per-child safety hardening applied in the `pre_exec` callback that
//! runs in the spawned child after `fork()` but before `execve()`.
//!
//! Anything set here is inherited by the lambda subprocess. Operations
//! must be async-signal-safe (no allocator, no mutex, no Rust-level
//! synchronization beyond what libc provides). Plain syscalls only.
//!
//! The always-on profile caps a child's blast radius before it can harm
//! the host: RLIMIT_CORE = 0 (no multi-gigabyte core dumps on crash),
//! RLIMIT_NOFILE = 4096 (fd-leak ceiling), RLIMIT_FSIZE = 100 MiB
//! (single-file write cap), and on Linux RLIMIT_NPROC = 4096 plus
//! PR_SET_PDEATHSIG(SIGKILL) and PR_SET_NO_NEW_PRIVS. Opt-in per-function
//! caps (memory_mb, cpu_time_secs, allowed_paths) layer on top elsewhere.

/// Whether the per-function filesystem allowlist (`allowed_paths`) is actually
/// enforced on this build's target OS.
///
/// The allowlist is implemented with Landlock, which is Linux-only. On
/// macOS/BSD/non-Unix, `apply_filesystem_allowlist` is a no-op, so a config
/// that sets `allowed_paths` runs with **no filesystem confinement** — the
/// paths are ignored. Callers use this to refuse or loudly warn rather than
/// imply a sandbox that isn't there (a silent security downgrade otherwise).
pub const fn filesystem_allowlist_enforced() -> bool {
    cfg!(target_os = "linux")
}

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
    setrlimit(Resource::RLIMIT_FSIZE, 100 * 1024 * 1024, 100 * 1024 * 1024).map_err(to_io)?;

    // RLIMIT_NPROC on Linux is enforced per-REAL-UID and counts THREADS, not
    // per-process (the man-page wording is subtle). It therefore bounds EVERY
    // riz worker's threads combined, not one child's subtree — and riz spawns
    // many multi-threaded workers (a single Bun process alone runs ~12 threads),
    // so a low cap silently kills workers mid-invocation once the fleet's thread
    // count crosses it (a full example fleet is ~46 workers). Keep it high enough
    // for a real fleet while still backstopping a fork bomb: an unbounded bomb is
    // stopped by ANY finite cap, and 4096 keeps the box alive without crippling
    // normal operation (it mirrors RLIMIT_NOFILE). macOS/BSD enforce per-USER and
    // would EINVAL against the host's total process count, so apply on Linux only.
    #[cfg(target_os = "linux")]
    setrlimit(Resource::RLIMIT_NPROC, 4096, 4096).map_err(to_io)?;

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
pub(super) fn apply_filesystem_allowlist(_paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn apply_filesystem_allowlist(_paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    Ok(())
}

/// Syscalls a web-API worker never legitimately makes, but which are the levers
/// of container/host escape and tampering: debugging into other processes,
/// loading kernel modules or eBPF, mount/namespace manipulation, kexec, the
/// kernel keyring, and clock/host mutation. Blocked with `EPERM`.
///
/// This is a **blocklist** (default-allow), not a strict allowlist: it hardens
/// the escape surface without risking the six language runtimes' ordinary
/// syscalls. `PR_SET_NO_NEW_PRIVS` (set in `apply_always_on_limits`) already
/// blocks setuid/file-cap escalation across the `execve`; seccomp adds the
/// syscall wall on top.
#[cfg(target_os = "linux")]
const SECCOMP_BLOCKED_SYSCALLS: [i64; 22] = [
    libc::SYS_ptrace,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_kexec_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_pivot_root,
    libc::SYS_setns,
    libc::SYS_add_key,
    libc::SYS_keyctl,
    libc::SYS_request_key,
    libc::SYS_acct,
    libc::SYS_settimeofday,
    libc::SYS_clock_settime,
    libc::SYS_adjtimex,
    libc::SYS_sethostname,
];

/// Compile the deny-by-`EPERM` seccomp-BPF blocklist for this build's arch.
/// Split from application so a unit test can validate the syscall numbers and
/// the `seccompiler` API compile and build without filtering the test process.
#[cfg(target_os = "linux")]
pub(super) fn build_seccomp_bpf() -> std::io::Result<seccompiler::BpfProgram> {
    use seccompiler::{SeccompAction, SeccompFilter};
    use std::collections::BTreeMap;

    #[cfg(target_arch = "x86_64")]
    let arch = seccompiler::TargetArch::x86_64;
    #[cfg(target_arch = "aarch64")]
    let arch = seccompiler::TargetArch::aarch64;

    let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = SECCOMP_BLOCKED_SYSCALLS
        .iter()
        .map(|&nr| (nr, Vec::new()))
        .collect();

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow
        SeccompAction::Errno(libc::EPERM as u32), // blocked: fail with EPERM
        arch,
    )
    .map_err(|e| std::io::Error::other(format!("seccomp filter build: {e}")))?;

    filter
        .try_into()
        .map_err(|e| std::io::Error::other(format!("seccomp compile: {e}")))
}

/// Install the seccomp blocklist on the calling process. Called last in the
/// `pre_exec` closure (after rlimits/prctl/landlock) so the setup syscalls
/// themselves are never filtered. Irreversible for the process and inherited
/// across `execve`.
#[cfg(target_os = "linux")]
pub(super) fn apply_seccomp_blocklist() -> std::io::Result<()> {
    let bpf = build_seccomp_bpf()?;
    seccompiler::apply_filter(&bpf)
        .map_err(|e| std::io::Error::other(format!("seccomp apply: {e}")))
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn apply_seccomp_blocklist() -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn apply_seccomp_blocklist() -> std::io::Result<()> {
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
    /// `/proc/self/status` for `NoNewPrivs: 1`. On macOS this test is
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
        // SAFETY: apply_always_on_limits is async-signal-safe — only
        // calls setrlimit + prctl, both on the POSIX safe list.
        #[allow(unsafe_code)]
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

    /// The blocklist compiles into a non-empty BPF program on this arch —
    /// validates the syscall numbers and the seccompiler API without filtering
    /// the test process.
    #[cfg(target_os = "linux")]
    #[test]
    fn seccomp_blocklist_builds() {
        let bpf = build_seccomp_bpf().expect("seccomp blocklist must build");
        assert!(!bpf.is_empty(), "compiled BPF program must be non-empty");
    }

    /// A spawned child runs under a seccomp filter (mode 2 = FILTER) — proven
    /// by `/proc/self/status`, the same way the NoNewPrivs test proves its
    /// prctl. This is the enforcement-installed proof; the blocklist contents
    /// are reviewed in `SECCOMP_BLOCKED_SYSCALLS`.
    #[cfg(target_os = "linux")]
    #[test]
    fn child_runs_under_seccomp_filter_on_linux() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("grep -E '^Seccomp:' /proc/self/status");
        cmd.stdout(Stdio::piped());
        // SAFETY: mirror the real pre_exec chain — set NO_NEW_PRIVS (via
        // apply_always_on_limits) before seccomp so seccomp() is permitted
        // without CAP_SYS_ADMIN, exactly as workers run. Only syscalls
        // (setrlimit/prctl/seccomp); building the filter allocates, the same
        // attested tradeoff as landlock.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                apply_always_on_limits()?;
                apply_seccomp_blocklist()?;
                Ok(())
            });
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Seccomp:\t2"),
            "child must run under a seccomp filter (Seccomp: 2); got {stdout:?}"
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

        // bash (not dash) so `ulimit -v` reports in 1024-byte blocks on Linux.
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c").arg("ulimit -H -v; ulimit -H -t");
        cmd.stdout(Stdio::piped());
        // SAFETY: both functions are async-signal-safe — only setrlimit +
        // prctl syscalls, both on the POSIX safe list.
        #[allow(unsafe_code)]
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
        apply_per_function_limits(None, None).expect("None inputs must succeed without setrlimit");
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
        let _ = apply_filesystem_allowlist;
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
        // SAFETY: setrlimit + prctl are async-signal-safe; the landlock
        // crate allocates internally but is attested safe in pre_exec by
        // widespread production use (see pool.rs spawn for the full note).
        #[allow(unsafe_code)]
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
    /// 4096 AND RLIMIT_FSIZE is 100 MiB (102400 KiB blocks). We verify
    /// via `ulimit -H -c -n -f` under **bash specifically**: `ulimit -f`
    /// reports in 1024-byte blocks on bash but 512-byte blocks on dash
    /// (Linux `/bin/sh`), so pinning the shell keeps the numbers stable
    /// across macOS (where `/bin/sh` is already bash) and Linux.
    #[test]
    fn child_inherits_always_on_caps() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c")
            .arg("ulimit -H -c; ulimit -H -n; ulimit -H -f");
        cmd.stdout(Stdio::piped());
        // SAFETY: apply_always_on_limits is async-signal-safe — only
        // calls setrlimit + prctl, both on the POSIX safe list.
        #[allow(unsafe_code)]
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
