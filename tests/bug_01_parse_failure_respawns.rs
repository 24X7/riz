//! BUG-01 regression: pipe desync on non-JSON lambda output.
//!
//! Trigger condition: a handler emits a stdout line that is not valid JSON.
//! Before the fix (`src/process/mod.rs` parse-failure arm), the bad-response
//! path returned an error but left the process alive with a desynced pipe —
//! subsequent requests on the same PID would read stale bytes (silent
//! cross-request data leak, P0).
//!
//! The fix calls `handle_process_failure()` which kills the process group and
//! respawns. This regression test proves the kill+respawn happened by spawning
//! a real subprocess (a tiny shell script that emits garbage to stdout) and
//! verifying the pool's PID changed after `invoke()` returned InvalidResponse.
//!
//! Would FAIL if someone removed `handle_process_failure` from the parse arm
//! (the PID would stay the same, exposing the desync regression).

use riz::config::{FunctionConfig, Protocol, RouteSpec, RuntimeKind};
use riz::process::runtime::RuntimeRegistry;
use riz::process::{PoolError, ProcessManager};
use riz::state::{LogEntry, RizState};
use riz::test_helpers::make_event;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Write a shell script that reads one stdin line (the riz envelope) and
/// echoes a non-JSON string to stdout, then exits. The TempDir guard must be
/// kept alive for the duration of the test — when it drops, the script is
/// deleted.
fn make_bad_response_script() -> (tempfile::TempDir, PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad-response.sh");
    let mut f = std::fs::File::create(&path).expect("create");
    writeln!(f, "#!/bin/sh\nread line\necho 'not json'\n").expect("write");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, path)
}

fn make_function_config(handler: PathBuf) -> FunctionConfig {
    FunctionConfig {
        runtime: RuntimeKind::Rust, // Rust runtime execs the handler binary directly
        protocol: Protocol::Http,
        handler,
        // Generous timeout: under heavy parallel test load the handler process
        // can be CPU-starved. If it doesn't emit its (malformed) line before
        // this deadline, invoke() returns Timeout instead of the InvalidResponse
        // this test asserts on. 15s gives starvation plenty of headroom while
        // still bounding a truly-hung process.
        timeout_ms: 15000,
        integration_timeout_ms: 15000,
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
async fn parse_failure_kills_and_respawns_the_process() {
    let (_dir_guard, script) = make_bad_response_script();
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

    // Capture initial PID.
    let initial_pid = {
        let stats = mgr.pool_stats().await;
        let badfn = stats
            .iter()
            .find(|p| p.name == "badfn")
            .expect("pool exists");
        assert_eq!(badfn.pids.len(), 1, "concurrency=1 → exactly one process");
        badfn.pids[0]
    };
    assert!(initial_pid > 0, "spawned process must have a real PID");

    // Invoke — script emits "not json", parse fails inside `invoke`.
    let event = make_event("GET", "/ping");
    let result = mgr.invoke("badfn", &event, 15000).await;

    match result {
        Err(PoolError::InvalidResponse(name, _)) => {
            assert_eq!(name, "badfn", "error must carry the function name");
        }
        other => panic!("expected InvalidResponse, got {other:?}"),
    }

    // The handle_process_failure call inside the parse-failure arm runs to
    // completion before invoke returns (it's awaited). So by the time we
    // observe pool_stats(), the new PID should already be in place.
    let new_pid = {
        let stats = mgr.pool_stats().await;
        let badfn = stats
            .iter()
            .find(|p| p.name == "badfn")
            .expect("pool exists");
        assert_eq!(
            badfn.pids.len(),
            1,
            "respawn must keep concurrency=1 invariant"
        );
        badfn.pids[0]
    };

    assert_ne!(
        initial_pid, new_pid,
        "BUG-01 regression: parse-failure arm must kill+respawn the process. \
         Same PID ({initial_pid}) twice indicates the pipe is desynced."
    );
    assert!(new_pid > 0, "respawned PID must be > 0");
}
