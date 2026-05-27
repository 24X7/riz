use crate::process::pool::{
    kill_process_group, spawn_with_cold_start_record, ProcessHandle, RoutePool, CRASH_THRESHOLD,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::error;
use tracing::warn;

pub(super) async fn handle_process_failure(
    pool: &Arc<RoutePool>,
    handle: &mut ProcessHandle,
    function_name: &str,
) {
    pool.restart_count.fetch_add(1, Ordering::Relaxed);
    let crashes = pool.consecutive_crashes.fetch_add(1, Ordering::Relaxed) + 1;
    if crashes >= CRASH_THRESHOLD {
        pool.healthy.store(false, Ordering::Relaxed);
        error!("function {function_name} marked unhealthy after {crashes} crashes");
    }
    kill_process_group(handle.pid);
    let _ = handle._child.kill().await;
    match spawn_with_cold_start_record(pool, function_name).await {
        Ok(new_handle) => {
            *handle = new_handle;
            pool.consecutive_crashes.store(0, Ordering::Relaxed);
        }
        Err(spawn_err) => {
            error!("failed to respawn {function_name}: {spawn_err}");
            pool.healthy.store(false, Ordering::Relaxed);
        }
    }
}

pub(super) fn spawn_liveness_watcher(
    pid: u32,
    handle_arc: Arc<Mutex<ProcessHandle>>,
    pool: Arc<RoutePool>,
    function_name: String,
) {
    if pid == 0 {
        return;
    }
    #[cfg(not(unix))]
    {
        return;
    }
    #[cfg(unix)]
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            use nix::sys::signal;
            use nix::unistd::Pid;
            if signal::kill(Pid::from_raw(pid as i32), None).is_err() {
                break;
            }
        }

        warn!("lambda process {pid} for {function_name} exited unexpectedly — respawning");
        let new_pid: Option<u32> = {
            if let Ok(mut guard) = handle_arc.try_lock() {
                if guard.pid == pid {
                    let _ = handle_process_failure(&pool, &mut guard, &function_name).await;
                    Some(guard.pid)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(new_pid) = new_pid {
            spawn_liveness_watcher(new_pid, handle_arc, pool, function_name);
        }
    });
}
