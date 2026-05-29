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

    /// Spawning a child via Command with the safety pre_exec must
    /// produce a process whose RLIMIT_CORE is 0. We verify by
    /// running `sh -c 'ulimit -c'` under the pre_exec — that shell
    /// builtin inherits the hard limit set during pre_exec.
    #[test]
    fn child_inherits_zero_core_limit() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("ulimit -c");
        cmd.stdout(Stdio::piped());
        // SAFETY: apply_always_on_limits is async-signal-safe — only
        // calls setrlimit, which is on the POSIX async-signal-safe list.
        unsafe {
            cmd.pre_exec(apply_always_on_limits);
        }
        let out = cmd.output().expect("spawn /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let trimmed = stdout.trim();
        assert_eq!(
            trimmed, "0",
            "child's RLIMIT_CORE must be 0 (got {trimmed:?})"
        );
    }
}
