//! The system route surface is complete and dynamic: every HTTP surface riz
//! mounts outside the user function set (probes, /_riz/* admin, the gateway,
//! the A2A agent) is visible in `riz routes` and the live `/_riz/registry` —
//! with its origin clearly system, not user. Conditional surfaces (gateway,
//! agent) appear only when configured.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

const FULL_CFG: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[gateway]
default_provider = "mock"
[gateway.providers.mock]
kind = "mock"

[agent]
name = "helper"
model = "mock"

[function.api]
runtime = "node"
handler = "index.handler"
"#;

const BARE_CFG: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[function.api]
runtime = "node"
handler = "index.handler"
"#;

fn run_routes(cfg: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("riz.toml"), cfg).unwrap();
    let out = Command::new(riz_binary())
        .current_dir(dir.path())
        .arg("routes")
        .output()
        .expect("riz routes");
    assert!(out.status.success(), "riz routes must exit 0");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn routes_command_lists_the_system_surface_with_origin() {
    let out = run_routes(FULL_CFG);

    // User functions keep their section, system surface gets its own —
    // origins obviously different.
    assert!(out.contains("api"), "user function listed: {out}");
    assert!(
        out.to_lowercase().contains("system"),
        "system origin named: {out}"
    );

    // The always-on surface.
    for route in [
        "GET /health",
        "GET /ready",
        "GET /_riz/health",
        "GET /_riz/metrics",
        "GET /_riz/registry",
        "POST /_riz/mcp",
        "GET /_riz/connections",
        "POST /deploy",
        "POST /cache/invalidate",
        "GET /openapi.json",
    ] {
        assert!(out.contains(route), "missing system route {route}: {out}");
    }
    // The conditional surface — configured here, so present.
    for route in [
        "POST /_riz/v1/chat/completions",
        "GET /_riz/v1/models",
        "POST /_riz/a2a",
        "GET /.well-known/agent-card.json",
    ] {
        assert!(
            out.contains(route),
            "missing conditional route {route}: {out}"
        );
    }
}

#[test]
fn routes_command_omits_unconfigured_surfaces() {
    let out = run_routes(BARE_CFG);
    assert!(
        !out.contains("/_riz/v1/"),
        "gateway routes must not appear without [gateway]: {out}"
    );
    assert!(
        !out.contains("/_riz/a2a"),
        "a2a routes must not appear without [agent]: {out}"
    );
    // The always-on surface still shows.
    assert!(out.contains("GET /_riz/health"), "{out}");
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0")
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/ready");
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(r) = reqwest::blocking::get(&url) {
            if r.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

/// The registry (which feeds the --dev TUI's function list) reports the
/// axum-mounted surfaces too, kind = "system".
#[test]
fn registry_reports_gateway_and_a2a_surfaces_as_system() {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    std::fs::write(
        dir.path().join("riz.toml"),
        FULL_CFG.replace("port = 0", &format!("port = {port}")),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("index.mjs"),
        "export const handler = async () => ({ statusCode: 200 });",
    )
    .unwrap();
    let mut child = Command::new(riz_binary())
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");
    assert!(wait_for_ready(port, Duration::from_secs(15)), "not ready");

    let reg: serde_json::Value =
        reqwest::blocking::get(format!("http://127.0.0.1:{port}/_riz/registry"))
            .expect("registry")
            .json()
            .expect("registry json");
    let _ = child.kill();
    let _ = child.wait();

    let functions = reg["functions"].as_array().expect("functions");
    let find = |name: &str| functions.iter().find(|f| f["name"] == name);

    let gw = find("_riz_gateway").unwrap_or_else(|| panic!("gateway surface missing: {reg}"));
    assert_eq!(gw["kind"], "system", "{reg}");
    assert!(
        gw["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "POST /_riz/v1/chat/completions"),
        "{reg}"
    );

    let a2a = find("_riz_a2a").unwrap_or_else(|| panic!("a2a surface missing: {reg}"));
    assert_eq!(a2a["kind"], "system", "{reg}");
    assert!(
        a2a["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "GET /.well-known/agent-card.json"),
        "{reg}"
    );

    let conns =
        find("_riz_connections").unwrap_or_else(|| panic!("connections surface missing: {reg}"));
    assert_eq!(conns["kind"], "system", "{reg}");
}

/// GET /openapi.json serves a valid OpenAPI 3.1 document autogenerated from the
/// live route table — one operation per declared route, with path params typed.
#[test]
fn openapi_json_serves_a_document_from_the_route_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let cfg = format!(
        r#"
[server]
port = {port}
host = "127.0.0.1"

[function.api]
runtime = "node"
handler = "index.handler"
[[function.api.routes]]
path = "/items/{{id}}"
method = "GET"
"#
    );
    std::fs::write(dir.path().join("riz.toml"), cfg).unwrap();
    std::fs::write(
        dir.path().join("index.mjs"),
        "export const handler = async () => ({ statusCode: 200 });",
    )
    .unwrap();
    let mut child = Command::new(riz_binary())
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");
    assert!(wait_for_ready(port, Duration::from_secs(15)), "not ready");

    let doc: serde_json::Value =
        reqwest::blocking::get(format!("http://127.0.0.1:{port}/openapi.json"))
            .expect("openapi request")
            .json()
            .expect("openapi json");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(doc["openapi"], "3.1.0", "{doc}");
    assert_eq!(doc["info"]["title"], "riz", "{doc}");
    let op = &doc["paths"]["/items/{id}"]["get"];
    assert!(op.is_object(), "operation for the declared route: {doc}");
    let param = &op["parameters"][0];
    assert_eq!(param["name"], "id", "path param typed: {doc}");
    assert_eq!(param["in"], "path", "{doc}");
    assert_eq!(param["required"], true, "{doc}");
}
