//! `riz scaffold static` — derive the agent-discovery surface from config (v2).
//!
//! See the v2 section of
//! `docs/superpowers/specs/2026-06-18-static-serving-design.md`.
//!
//! Two layers:
//!   1. the pure generators (`scaffold::generate_*`) — every function becomes a
//!      tool entry with its runtime + routes; the JSON is valid and carries the
//!      MCP endpoint.
//!   2. the round-trip that ties v2 to v1: scaffold into a dir, point `[static]`
//!      at it, and prove `static_files::serve` returns the generated `llms.txt`
//!      and `.well-known/riz.json` — i.e. a live instance describes itself.
//!   3. the CLI (`riz scaffold static`) as a subprocess: files written, --wire
//!      edits riz.toml to a still-valid config, --force / no-clobber behavior.
//!
//! Run: `cargo nextest run --test static_scaffold`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use http::{HeaderMap, Method, StatusCode};

const SAMPLE: &str = r#"
[server]
port = 0
host = "127.0.0.1"

[function.orders]
runtime = "node"
handler = "index.handler"

[[function.orders.routes]]
path = "/orders/{id}"
method = "GET"

[function.health]
runtime = "python"
handler = "app.check"
"#;

fn sample_config() -> riz::config::Config {
    toml::from_str(SAMPLE).expect("sample config parses")
}

// ───────────────────────────── pure generators ──────────────────────────────

/// WebSocket functions are callable tools via ephemeral sessions, so the
/// generated agent-discovery files advertise them with the session
/// description — mirroring the live /_riz/mcp tools/list behavior.
#[test]
fn websocket_functions_are_advertised_as_session_tools() {
    let cfg: riz::config::Config = toml::from_str(
        r#"
[function.orders]
runtime = "node"
handler = "orders.handler"

[function.chat]
runtime = "bun"
protocol = "websocket"
handler = "chat.handler"
[[function.chat.routes]]
path = "/ws"
method = "GET"
"#,
    )
    .expect("config parses");

    let txt = riz::scaffold::generate_llms_txt(&cfg);
    assert!(txt.contains("### orders"), "http tool missing:\n{txt}");
    assert!(
        txt.contains("### chat"),
        "WS session tool must appear in llms.txt:\n{txt}"
    );
    assert!(
        txt.contains("ephemeral WebSocket session"),
        "session semantics must be named:\n{txt}"
    );

    let json = riz::scaffold::generate_well_known(&cfg);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let names: Vec<&str> = v["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"orders") && names.contains(&"chat"),
        "both tools advertised: {v}"
    );
}

#[test]
fn llms_txt_lists_every_function_as_a_tool_with_routes_and_runtime() {
    let cfg = sample_config();
    let txt = riz::scaffold::generate_llms_txt(&cfg);

    // Both functions appear as tool sections.
    assert!(txt.contains("### orders"), "orders tool missing:\n{txt}");
    assert!(txt.contains("### health"), "health tool missing");
    // Declared route is shown verbatim.
    assert!(txt.contains("GET /orders/{id}"), "orders route missing");
    // A function with no [[routes]] gets the implicit ANY /<name> fallback.
    assert!(txt.contains("/health"), "health implicit route missing");
    // Runtimes are surfaced.
    assert!(
        txt.contains("`node`") && txt.contains("`python`"),
        "runtimes missing"
    );
    // The MCP endpoint is advertised so an agent knows where to call.
    assert!(txt.contains("/_riz/mcp"), "mcp endpoint missing");
}

#[test]
fn well_known_is_valid_json_with_tools_and_mcp_endpoint() {
    let cfg = sample_config();
    let json = riz::scaffold::generate_well_known(&cfg);
    let v: serde_json::Value =
        serde_json::from_str(&json).expect("generated riz.json is valid JSON");

    assert_eq!(v["mcp"]["endpoint"], "/_riz/mcp");
    let tools = v["tools"].as_array().expect("tools is an array");
    assert_eq!(tools.len(), 2, "both functions become tools");

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"orders") && names.contains(&"health"));

    let orders = tools.iter().find(|t| t["name"] == "orders").unwrap();
    assert_eq!(orders["runtime"], "node");
    assert_eq!(orders["routes"][0]["method"], "GET");
    assert_eq!(orders["routes"][0]["path"], "/orders/{id}");
    assert!(orders["description"].as_str().unwrap().contains("orders"));
}

#[test]
fn mcp_description_override_flows_into_the_generated_tool() {
    let cfg: riz::config::Config = toml::from_str(
        r#"
[server]
port = 0
host = "127.0.0.1"

[function.search]
runtime = "bun"
handler = "index.handler"

[function.search.mcp]
description = "Full-text search over the catalog."
"#,
    )
    .unwrap();
    let json = riz::scaffold::generate_well_known(&cfg);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["tools"][0]["description"],
        "Full-text search over the catalog."
    );
}

// ─────────────────────── round-trip: instance serves itself ─────────────────

#[tokio::test]
async fn scaffolded_files_are_served_by_a_static_instance() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = sample_config();

    // Write a riz.toml so --wire has something to edit, then scaffold + wire.
    let config_path = dir.path().join("riz.toml");
    fs::write(&config_path, SAMPLE).unwrap();
    let site = dir.path().join("public");
    let opts = riz::scaffold::ScaffoldOptions {
        dir: site.clone(),
        mount: "/".to_string(),
        wire: true,
        force: false,
    };
    let result = riz::scaffold::scaffold_static(&cfg, &config_path, &opts).expect("scaffold");
    assert!(result.wired, "[static] should have been wired in");
    assert!(site.join("llms.txt").is_file());
    assert!(site.join(".well-known/riz.json").is_file());

    // The wired config must still parse AND validate (dir exists now).
    let wired: riz::config::Config =
        toml::from_str(&fs::read_to_string(&config_path).unwrap()).expect("wired config parses");
    wired.validate().expect("wired config validates");
    let static_cfg = wired
        .static_site
        .clone()
        .expect("[static] present after wire");

    // A live instance with this [static] serves its own discovery files.
    let resp = riz::static_files::serve(&Method::GET, "/llms.txt", &HeaderMap::new(), &static_cfg)
        .await
        .expect("llms.txt served");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("### orders"));

    let resp = riz::static_files::serve(
        &Method::GET,
        "/.well-known/riz.json",
        &HeaderMap::new(),
        &static_cfg,
    )
    .await
    .expect("riz.json served");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["mcp"]["endpoint"], "/_riz/mcp");
}

// ─────────────────────────── overwrite semantics ────────────────────────────

#[test]
fn refuses_to_clobber_without_force_then_overwrites_with_force() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = sample_config();
    let config_path = dir.path().join("riz.toml");
    fs::write(&config_path, SAMPLE).unwrap();
    let site = dir.path().join("public");

    let opts = |force: bool| riz::scaffold::ScaffoldOptions {
        dir: site.clone(),
        mount: "/".to_string(),
        wire: false,
        force,
    };

    // First write succeeds.
    riz::scaffold::scaffold_static(&cfg, &config_path, &opts(false)).unwrap();
    // Second without --force refuses.
    let err = riz::scaffold::scaffold_static(&cfg, &config_path, &opts(false)).unwrap_err();
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "got: {err}"
    );
    // With --force it succeeds.
    riz::scaffold::scaffold_static(&cfg, &config_path, &opts(true)).unwrap();
}

#[test]
fn wire_is_idempotent_when_static_already_configured() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("public");
    let toml = format!(
        "{SAMPLE}\n[static]\ndir = {:?}\n",
        site.display().to_string()
    );
    let cfg: riz::config::Config = toml::from_str(&toml).unwrap();
    let config_path = dir.path().join("riz.toml");
    fs::write(&config_path, &toml).unwrap();

    let opts = riz::scaffold::ScaffoldOptions {
        dir: site.clone(),
        mount: "/".to_string(),
        wire: true,
        force: true,
    };
    let result = riz::scaffold::scaffold_static(&cfg, &config_path, &opts).unwrap();
    assert!(!result.wired, "must not add a second [static] block");
    // The config text still has exactly one [static] occurrence.
    let text = fs::read_to_string(&config_path).unwrap();
    assert_eq!(text.matches("[static]").count(), 1);
}

// ───────────────────────────── CLI subprocess ───────────────────────────────

fn riz_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target_dir.join("debug").join("riz")
}

#[test]
fn cli_scaffold_static_writes_files_and_wires_config() {
    let bin = riz_binary();
    if !bin.exists() {
        eprintln!("SKIP: riz binary not built at {}", bin.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("riz.toml"), SAMPLE).unwrap();

    let out = Command::new(&bin)
        .current_dir(dir.path())
        .args(["scaffold", "static", "public", "--wire"])
        .output()
        .expect("spawn riz scaffold static");
    assert!(
        out.status.success(),
        "scaffold failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(dir.path().join("public/llms.txt").is_file());
    assert!(dir.path().join("public/.well-known/riz.json").is_file());

    // The edited riz.toml must still be a valid config with [static] set.
    let text = fs::read_to_string(dir.path().join("riz.toml")).unwrap();
    assert!(text.contains("[static]"), "riz.toml not wired:\n{text}");
    let cfg: riz::config::Config = toml::from_str(&text).expect("wired config parses");
    assert!(cfg.static_site.is_some());

    // Re-running without --force refuses to clobber (non-zero exit).
    let out2 = Command::new(&bin)
        .current_dir(dir.path())
        .args(["scaffold", "static", "public"])
        .output()
        .unwrap();
    assert!(
        !out2.status.success(),
        "second run should refuse to overwrite"
    );
    assert!(
        String::from_utf8_lossy(&out2.stderr).contains("--force")
            || String::from_utf8_lossy(&out2.stdout).contains("--force")
    );
}

#[test]
fn cli_scaffold_static_errors_clearly_without_a_config() {
    let bin = riz_binary();
    if !bin.exists() {
        eprintln!("SKIP: riz binary not built");
        return;
    }
    let dir = tempfile::tempdir().unwrap(); // no riz.toml
    let out = Command::new(&bin)
        .current_dir(dir.path())
        .args(["scaffold", "static"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "should fail without a config");
    assert!(String::from_utf8_lossy(&out.stderr)
        .to_lowercase()
        .contains("riz.toml"));
}
