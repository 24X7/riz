//! BUG-01 regression (Runtime-API era): an unresponsive worker must be
//! killed + respawned, never left to wedge the pool.
//!
//! Original BUG-01 was a stdout pipe-desync on non-JSON output. With compiled
//! runtimes now speaking the AWS Lambda Runtime API (not stdin/stdout), the
//! analogous hazard is a worker that connects but never answers an invocation
//! (a hung or broken official runtime client). `invoke()` must time out and
//! `handle_process_failure()` must kill + respawn the worker so the next
//! request gets a fresh, healthy process — not a stuck PID.
//!
//! Would FAIL if the runtime-api timeout arm stopped killing + respawning (the
//! PID would stay the same, exposing a wedged worker).

use riz::config::{FunctionConfig, Protocol, RouteSpec, RuntimeKind};
use riz::process::runtime::RuntimeRegistry;
use riz::process::{PoolError, ProcessManager};
use riz::state::{LogEntry, RizState};
use riz::test_helpers::make_event;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A "handler" that never speaks the Runtime API — it just sleeps, modelling a
/// hung/broken official runtime client. riz must time out and respawn it.
fn make_unresponsive_script() -> (tempfile::TempDir, PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hung-handler.sh");
    let mut f = std::fs::File::create(&path).expect("create");
    // Stay alive (so it's not a crash-respawn storm) but never poll the
    // Runtime API → every invocation times out.
    writeln!(f, "#!/bin/sh\nsleep 60\n").expect("write");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, path)
}

fn make_function_config(handler: PathBuf) -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Rust, // compiled runtime → AWS Runtime API transport
        protocol: Protocol::Http,
        handler,
        timeout_ms: 2000,
        integration_timeout_ms: 2000,
        stage_variables: Default::default(),
        cache_ttl_secs: None,
        concurrency: 1,
        routes: vec![RouteSpec {
            path: "/ping".into(),
            method: "GET".into(),
        }],
        cors: None,
        authorizer: None,
        memory_mb: None,
        cpu_time_secs: None,
        allowed_paths: None,
        mcp: None,
        capabilities: Default::default(),
        guard_in: None,
        guard_out: None,
    }
}

#[tokio::test]
async fn unresponsive_worker_is_killed_and_respawned() {
    let (_dir_guard, script) = make_unresponsive_script();
    let cfg = make_function_config(script);

    let registry = Arc::new(RuntimeRegistry::new().expect("registry init"));
    let riz_state = Arc::new(RizState::new());
    let (log_tx, _log_rx) = mpsc::channel::<LogEntry>(16);

    let mgr = ProcessManager::new(riz_state);
    let mut functions = indexmap::IndexMap::new();
    functions.insert("badfn".to_string(), cfg);
    mgr.spawn_all(&functions, &registry, log_tx)
        .await
        .expect("spawn_all");

    let initial_pid = {
        let stats = mgr.pool_stats().await;
        let badfn = stats.iter().find(|p| p.name == "badfn").expect("pool exists");
        assert_eq!(badfn.pids.len(), 1, "concurrency=1 → exactly one process");
        badfn.pids[0]
    };
    assert!(initial_pid > 0, "spawned process must have a real PID");

    // Invoke — the worker never answers the Runtime API, so invoke times out.
    let event = make_event("GET", "/ping");
    let result = mgr.invoke("badfn", &event, 2000).await;
    match result {
        Err(PoolError::Timeout(name, _)) => {
            assert_eq!(name, "badfn", "error must carry the function name");
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    // The timeout arm kills + respawns before invoke returns (it's awaited).
    let new_pid = {
        let stats = mgr.pool_stats().await;
        let badfn = stats.iter().find(|p| p.name == "badfn").expect("pool exists");
        assert_eq!(badfn.pids.len(), 1, "respawn must keep concurrency=1");
        badfn.pids[0]
    };

    assert_ne!(
        initial_pid, new_pid,
        "BUG-01 regression: the timeout arm must kill+respawn the worker. \
         Same PID ({initial_pid}) twice indicates a wedged worker."
    );
    assert!(new_pid > 0, "respawned PID must be > 0");
}
