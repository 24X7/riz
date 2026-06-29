//! WASM guards e2e (v1 roadmap #3 + #4) — one `.wasm` guard, every runtime.
//!
//! Spawns the REAL `riz run` binary with the guard fixture
//! (tests/fixtures/guard-wasm) wrapping handlers on DIFFERENT runtimes:
//!
//! guard_in (Bun + WASM handlers, same guard module):
//!   * allow → handler runs normally;
//!   * deny  → the guard's status/body come back, the handler never runs;
//!   * mutate → the handler observably receives the scrubbed event;
//!   * garbage verdict → 502, fail closed — a broken policy never allows.
//!
//! guard_out (Bun handler):
//!   * a response carrying an SSN is redacted before bytes leave;
//!   * a response carrying `deny-me` is replaced with the guard's status.
//!
//! Guard timing: /_riz/health lists the `{fn}::guard_in` entry with
//! invocation counts after traffic.
//!
//! SKIPS cleanly without bun or the guard wasm artifact
//! (`cargo build --release --target wasm32-wasip1` in tests/fixtures/guard-wasm).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn guard_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/guard-wasm/target/wasm32-wasip1/release/guard-wasm.wasm")
}

fn echo_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/lambdas/echo-wasm/target/wasm32-wasip1/release/echo-wasm.wasm")
}

fn bun_available() -> bool {
    Command::new("bun").arg("--version").output().is_ok()
}

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if let Ok(resp) = reqwest::blocking::get(format!("http://127.0.0.1:{port}/ready")) {
            if resp.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Boot riz with the guard fixture wrapping a bun echo (guard_in + guard_out)
/// and — when its artifact exists — a wasm echo (guard_in), proving the SAME
/// guard module protects different runtimes.
fn boot(port: u16, dir: &std::path::Path, with_wasm_echo: bool) -> Server {
    let guard = guard_wasm();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut toml = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[function.echo_bun]
runtime = "bun"
handler = "{repo}/examples/lambdas/echo-bun/index.ts"
timeout_ms = 5000
concurrency = 2
guard_in = "{guard}"

[[function.echo_bun.routes]]
path = "/echo-bun"
method = "ANY"

[function.redact_bun]
runtime = "bun"
handler = "{repo}/examples/lambdas/echo-bun/index.ts"
timeout_ms = 5000
concurrency = 1
guard_out = "{guard}"

[[function.redact_bun.routes]]
path = "/redact-bun"
method = "ANY"
"#,
        repo = repo.display(),
        guard = guard.display(),
    );
    if with_wasm_echo {
        toml.push_str(&format!(
            r#"
[function.echo_wasm]
runtime = "wasm"
handler = "{wasm}"
timeout_ms = 5000
concurrency = 1
guard_in = "{guard}"

[[function.echo_wasm.routes]]
path = "/echo-wasm"
method = "ANY"
"#,
            wasm = echo_wasm().display(),
            guard = guard.display(),
        ));
    }
    let cfg = dir.join("riz.toml");
    std::fs::write(&cfg, toml).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_riz"))
        .args(["--config", cfg.to_str().unwrap(), "run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riz run");
    Server(child)
}

#[tokio::test(flavor = "multi_thread")]
async fn one_wasm_guard_protects_every_runtime() {
    if !guard_wasm().exists() {
        eprintln!("wasm_guards: guard fixture not built — skipping");
        return;
    }
    if !bun_available() {
        eprintln!("wasm_guards: bun not on PATH — skipping");
        return;
    }
    let with_wasm = echo_wasm().exists();
    if !with_wasm {
        eprintln!("wasm_guards: echo-wasm artifact missing — wasm-runtime leg skipped");
    }

    let port = pick_free_port();
    let dir = tempfile::TempDir::new().unwrap();
    let server = boot(port, dir.path(), with_wasm);
    let ready = tokio::task::spawn_blocking(move || wait_for_ready(port, Duration::from_secs(30)))
        .await
        .unwrap();
    assert!(ready, "riz never became ready");
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // ── guard_in: allow → handler runs ──────────────────────────────────
    // The first request can race the guard pool's warm-up on a slow CI runner:
    // a not-yet-ready guard `__wasm-host` fails closed (502). Retry briefly
    // until the guard is warm (this only tolerates cold start — a guard that
    // truly can't serve stays non-200 and the assert below still fails).
    let resp = {
        let mut attempt = 0;
        loop {
            let r = client.get(format!("{base}/echo-bun")).send().await.unwrap();
            if r.status() == 200 || attempt >= 20 {
                break r;
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["echo"], "/echo-bun", "handler must have run: {body}");

    // ── guard_in: deny → guard status, handler never runs ───────────────
    let resp = client
        .get(format!("{base}/echo-bun"))
        .header("x-guard", "deny")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 451, "guard's chosen status comes back");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["reason"], "guard denied", "{body}");
    assert!(body.get("echo").is_none(), "handler must NOT have run");

    // ── guard_in: mutate → handler sees the scrubbed event ──────────────
    let resp = client
        .get(format!("{base}/echo-bun"))
        .header("x-guard", "mutate")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["requestHeaders"]["x-guard-mutated"], "yes",
        "handler must receive the guard-mutated event: {body}"
    );

    // ── guard_in: garbage verdict → fail CLOSED ──────────────────────────
    let resp = client
        .get(format!("{base}/echo-bun"))
        .header("x-guard", "garbage")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "broken guard must fail closed, got {}",
        resp.status()
    );

    // ── Cross-runtime: the SAME guard module wraps a WASM handler ───────
    if with_wasm {
        let resp = client
            .get(format!("{base}/echo-wasm"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "wasm handler allow path");
        let resp = client
            .get(format!("{base}/echo-wasm"))
            .header("x-guard", "deny")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 451, "same guard denies on the wasm runtime");
    }

    // ── guard_out: SSN redacted before bytes leave ───────────────────────
    // echo-bun reflects the request body into its response body, so a body
    // carrying an SSN produces a RESPONSE carrying an SSN — which guard_out
    // must scrub.
    let resp = client
        .post(format!("{base}/redact-bun"))
        .body(r#"{"name":"jane","ssn":"123-45-6789"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(
        !text.contains("123-45-6789"),
        "SSN must not leave the server: {text}"
    );
    assert!(text.contains("***-**-****"), "redaction marker: {text}");

    // ── guard_out: deny replaces the response ────────────────────────────
    let resp = client
        .post(format!("{base}/redact-bun"))
        .body("please deny-me now")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 451);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["reason"], "response blocked", "{body}");

    // ── Guard timing surfaces in /_riz/health ────────────────────────────
    let health: serde_json::Value = client
        .get(format!("{base}/_riz/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let functions = health["functions"].as_array().unwrap();
    let guard_entry = functions
        .iter()
        .find(|f| f["name"] == "echo_bun::guard_in")
        .unwrap_or_else(|| panic!("health must list the guard pool: {functions:?}"));
    assert!(
        guard_entry["invocations"].as_u64().unwrap_or(0) >= 3,
        "guard invocations recorded: {guard_entry}"
    );

    drop(server);
}
