//! `examples/typescript-todo` — a full app (todo API + Vite/React client) on
//! one riz binary, one origin. See the example's README.
//!
//! Two layers:
//!   1. a CONTRACT test (always runs, no toolchain) pinning the example's
//!      wiring: the riz.toml functions/routes, the `[static]` block, the
//!      committed build output, and the handler shape. This is the drift guard.
//!   2. a COLOCATION integration test (gated on `bun`) that boots the actual
//!      `riz` binary against the example and proves the API CRUD and the static
//!      client are served on the SAME origin with no CORS — the whole point.
//!
//! Run: `cargo nextest run --test example_typescript_todo`

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/typescript-todo")
}

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

fn read(rel: &str) -> String {
    let p = example_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ───────────────────────────── contract / drift guard ───────────────────────

#[test]
fn the_example_is_wired_for_colocation() {
    // riz.toml parses as a real riz Config.
    let cfg: riz::config::Config =
        toml::from_str(&read("riz.toml")).expect("example riz.toml parses as a riz Config");

    // One function, four routes — the AWS "one pool, many routes" shape.
    let todos = cfg.functions.get("todos").expect("todos function declared");
    assert_eq!(todos.runtime.as_str(), "bun");
    let routes: Vec<(String, String)> = todos
        .effective_routes("todos")
        .into_iter()
        .map(|r| (r.method, r.path))
        .collect();
    for expected in [
        ("GET", "/api/todos"),
        ("POST", "/api/todos"),
        ("PATCH", "/api/todos/{id}"),
        ("DELETE", "/api/todos/{id}"),
    ] {
        assert!(
            routes
                .iter()
                .any(|(m, p)| m == expected.0 && p == expected.1),
            "missing route {expected:?}; have {routes:?}"
        );
    }

    // The [static] block serves the client build on the same origin.
    let st = cfg.static_site.as_ref().expect("[static] block present");
    assert!(st.dir.ends_with("client/dist"), "static dir = {:?}", st.dir);
    assert_eq!(st.mount, "/");
    assert!(st.spa_fallback, "spa_fallback should be on for the SPA");

    // The committed build output exists so `riz run` works out of the box.
    assert!(
        example_dir().join("client/dist/index.html").is_file(),
        "client/dist/index.html missing — run `cd client && bun run build`"
    );

    // The handler exports `handler`, and the client is a real Vite project.
    assert!(read("api/todos.ts").contains("export const handler"));
    let pkg = read("client/package.json");
    assert!(pkg.contains("\"build\"") && pkg.contains("vite"));
    assert!(pkg.contains("react"));
}

// ───────────────────────────── colocation integration ───────────────────────

fn bun_available() -> bool {
    Command::new("bun").arg("--version").output().is_ok()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the riz child on drop so a panicking assertion never leaks a server.
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn api_and_client_are_served_on_one_origin() {
    if !bun_available() {
        eprintln!("SKIP: bun not on PATH");
        return;
    }
    let bin = riz_binary();
    if !bin.exists() {
        eprintln!("SKIP: riz binary not built at {}", bin.display());
        return;
    }

    let port = free_port();
    let child = Command::new(&bin)
        .args(["--port", &port.to_string(), "run"])
        .current_dir(example_dir())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz run");
    let _server = Server(child);

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::new();

    // Wait for readiness.
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if client.get(format!("{base}/_riz/health")).send().is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "riz did not come up on {base}");
        std::thread::sleep(Duration::from_millis(150));
    }

    // GET / → the built React client (static), same origin.
    let resp = client.get(format!("{base}/")).send().unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let html = resp.text().unwrap();
    assert!(
        html.contains("<div id=\"root\">"),
        "served HTML is not the client shell"
    );

    // The API (a function) answers on the same origin — no CORS, no second host.
    let list = client.get(format!("{base}/api/todos")).send().unwrap();
    assert_eq!(list.status(), 200);
    assert_eq!(
        list.text().unwrap().trim(),
        "[]",
        "fresh store should be empty"
    );

    // Create.
    let created = client
        .post(format!("{base}/api/todos"))
        .body(r#"{"title":"ship riz"}"#)
        .send()
        .unwrap();
    assert_eq!(created.status(), 201);
    let v: serde_json::Value = created.json().unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["title"], "ship riz");
    assert_eq!(v["completed"], false);

    // Toggle via PATCH.
    let patched = client
        .patch(format!("{base}/api/todos/{id}"))
        .body(r#"{"completed":true}"#)
        .send()
        .unwrap();
    assert_eq!(patched.status(), 200);
    assert_eq!(
        patched.json::<serde_json::Value>().unwrap()["completed"],
        true
    );

    // Delete → 204.
    let deleted = client
        .delete(format!("{base}/api/todos/{id}"))
        .send()
        .unwrap();
    assert_eq!(deleted.status(), 204);

    // SPA fallback: an unknown client-side route returns the app shell, not 404.
    let spa = client
        .get(format!("{base}/some/client/route"))
        .send()
        .unwrap();
    assert_eq!(spa.status(), 200);
    assert!(spa
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));

    // Precedence: a missing API method on the function's path is the FUNCTION's
    // response (405), never the static layer.
    let wrong = client.put(format!("{base}/api/todos")).send().unwrap();
    assert_ne!(wrong.status(), 200, "static must not answer an API path");
}
