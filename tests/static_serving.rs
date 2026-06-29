//! Static-file serving — colocate a site with the API on one binary.
//!
//! See `docs/superpowers/specs/2026-06-18-static-serving-design.md`.
//!
//! Two layers are exercised:
//!   1. `static_files::serve()` directly — the file resolver / response builder
//!      (content-type, ETag/304, range/206, cache policy, dotfile + traversal
//!      safety, directory→index, SPA fallback, the agent-discovery files).
//!   2. The full `dispatch_lambda` path via `build_app` + a mock function — the
//!      precedence keystone: a function always wins over a static file at the
//!      same path.
//!
//! Run: `cargo nextest run --test static_serving`

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use http::{header, HeaderMap, HeaderValue, Method, StatusCode};

// ───────────────────────────── helpers ──────────────────────────────────────

/// Parse a `[static]` config rooted at `dir`, plus any extra `[static]` lines.
fn static_cfg(dir: &Path, extra: &str) -> riz::config::StaticConfig {
    let toml = format!(
        "[server]\nport = 0\nhost = \"127.0.0.1\"\n\n[static]\ndir = {:?}\n{extra}",
        dir.display()
    );
    let config: riz::config::Config = toml::from_str(&toml).expect("config parses");
    config.static_site.expect("[static] present")
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec()
}

fn get(path: &str) -> (Method, &str, HeaderMap) {
    (Method::GET, path, HeaderMap::new())
}

async fn serve(
    method: Method,
    path: &str,
    headers: HeaderMap,
    cfg: &riz::config::StaticConfig,
) -> Option<axum::response::Response> {
    riz::static_files::serve(&method, path, &headers, cfg).await
}

// ───────────────────────────── index / directory ────────────────────────────

#[tokio::test]
async fn serves_index_for_root_and_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("index.html"), "<h1>root</h1>").unwrap();
    fs::create_dir(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/index.html"), "<h1>docs</h1>").unwrap();
    let cfg = static_cfg(dir.path(), "");

    // "/" → root index.
    let (m, p, h) = get("/");
    let resp = serve(m, p, h, &cfg).await.expect("served root");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert_eq!(body_bytes(resp).await, b"<h1>root</h1>");

    // "/docs/" → directory index.
    let (m, p, h) = get("/docs/");
    let resp = serve(m, p, h, &cfg).await.expect("served docs index");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, b"<h1>docs</h1>");
}

#[tokio::test]
async fn directory_without_index_is_404_not_a_listing() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    fs::write(dir.path().join("assets/secret.txt"), "shh").unwrap();
    let cfg = static_cfg(dir.path(), "");

    let (m, p, h) = get("/assets/");
    let resp = serve(m, p, h, &cfg).await.expect("owns response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Never a directory listing.
    let body = String::from_utf8(body_bytes(resp).await).unwrap();
    assert!(!body.contains("secret.txt"), "must not list the directory");
}

// ───────────────────────────── security keystones ───────────────────────────

#[tokio::test]
async fn path_traversal_dotdot_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // A secret sibling OUTSIDE the served dir.
    let outside = dir.path().parent().unwrap().join("riz-traversal-secret.txt");
    fs::write(&outside, "TOPSECRET").unwrap();
    let root = dir.path().join("public");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("index.html"), "ok").unwrap();
    let cfg = static_cfg(&root, "");

    for attack in [
        "/../riz-traversal-secret.txt",
        "/..%2friz-traversal-secret.txt",
        "/%2e%2e/riz-traversal-secret.txt",
        "/foo/../../riz-traversal-secret.txt",
    ] {
        let (m, p, h) = get(attack);
        let resp = serve(m, p, h, &cfg).await.expect("owns response");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "traversal {attack:?} must 404"
        );
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(!body.contains("TOPSECRET"), "{attack:?} leaked the secret file");
    }
    let _ = fs::remove_file(&outside);
}

#[tokio::test]
async fn symlink_escaping_the_root_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.txt");
    fs::write(&outside, "ESCAPED").unwrap();
    let root = dir.path().join("public");
    fs::create_dir(&root).unwrap();
    let link = root.join("link.txt");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let cfg = static_cfg(&root, "");
        let (m, p, h) = get("/link.txt");
        let resp = serve(m, p, h, &cfg).await.expect("owns response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "symlink escape must 404");
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(!body.contains("ESCAPED"));
    }
}

#[tokio::test]
async fn dotfiles_are_hidden_except_well_known() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".git/config"), "[core]").unwrap();
    fs::create_dir(dir.path().join(".well-known")).unwrap();
    fs::write(dir.path().join(".well-known/riz.json"), "{\"ok\":true}").unwrap();
    let cfg = static_cfg(dir.path(), "");

    for hidden in ["/.env", "/.git/config"] {
        let (m, p, h) = get(hidden);
        let resp = serve(m, p, h, &cfg).await.expect("owns response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{hidden:?} must be hidden");
    }

    // The agent surface IS served.
    let (m, p, h) = get("/.well-known/riz.json");
    let resp = serve(m, p, h, &cfg).await.expect("owns response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, b"{\"ok\":true}");
}

#[tokio::test]
async fn riz_system_path_is_never_served_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("_riz")).unwrap();
    fs::write(dir.path().join("_riz/mcp"), "FAKE_MCP").unwrap();
    let cfg = static_cfg(dir.path(), "");

    // serve() returns None for /_riz/* so the caller never serves the file and
    // the real system route owns the path.
    assert!(
        serve(Method::GET, "/_riz/mcp", HeaderMap::new(), &cfg).await.is_none(),
        "/_riz/mcp must never resolve to a disk file"
    );
    assert!(
        serve(Method::GET, "/_riz", HeaderMap::new(), &cfg).await.is_none(),
        "/_riz must never resolve to a disk file"
    );
}

// ───────────────────────────── content negotiation ──────────────────────────

#[tokio::test]
async fn content_type_is_correct_for_wasm_json_svg() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("m.wasm"), b"\0asm").unwrap();
    fs::write(dir.path().join("d.json"), b"{}").unwrap();
    fs::write(dir.path().join("i.svg"), b"<svg/>").unwrap();
    fs::write(dir.path().join("l.txt"), b"hi").unwrap();
    let cfg = static_cfg(dir.path(), "");

    for (path, expect) in [
        ("/m.wasm", "application/wasm"),
        ("/d.json", "application/json"),
        ("/i.svg", "image/svg+xml"),
        ("/l.txt", "text/plain"),
    ] {
        let (m, p, h) = get(path);
        let resp = serve(m, p, h, &cfg).await.expect("served");
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(ct.starts_with(expect), "{path}: expected {expect}, got {ct}");
    }
}

// ───────────────────────────── conditional / range ──────────────────────────

#[tokio::test]
async fn conditional_request_returns_304() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.js"), b"console.log(1)").unwrap();
    let cfg = static_cfg(dir.path(), "");

    // First request → 200 + ETag.
    let (m, p, h) = get("/app.js");
    let resp = serve(m, p, h, &cfg).await.expect("served");
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers().get(header::ETAG).unwrap().clone();

    // Re-request with If-None-Match → 304, empty body.
    let mut headers = HeaderMap::new();
    headers.insert(header::IF_NONE_MATCH, etag);
    let resp = serve(Method::GET, "/app.js", headers, &cfg).await.expect("served");
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert!(body_bytes(resp).await.is_empty(), "304 has no body");
}

#[tokio::test]
async fn range_request_returns_206() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("v.bin"), b"0123456789").unwrap();
    let cfg = static_cfg(dir.path(), "");

    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-5"));
    let resp = serve(Method::GET, "/v.bin", headers, &cfg).await.expect("served");
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers().get(header::CONTENT_RANGE).unwrap().to_str().unwrap(),
        "bytes 2-5/10"
    );
    assert_eq!(body_bytes(resp).await, b"2345");

    // Unsatisfiable range → 416.
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=50-60"));
    let resp = serve(Method::GET, "/v.bin", headers, &cfg).await.expect("served");
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

// ───────────────────────────── HEAD ─────────────────────────────────────────

#[tokio::test]
async fn head_returns_headers_no_body() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("page.html"), "<h1>hi</h1>").unwrap();
    let cfg = static_cfg(dir.path(), "");

    let resp = serve(Method::HEAD, "/page.html", HeaderMap::new(), &cfg)
        .await
        .expect("served");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_LENGTH).unwrap().to_str().unwrap(),
        "11"
    );
    assert!(body_bytes(resp).await.is_empty(), "HEAD has no body");
}

// ───────────────────────────── cache policy ─────────────────────────────────

#[tokio::test]
async fn immutable_cache_header_on_hash_named_asset_no_cache_on_html() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.4f1c2a9b.js"), "x").unwrap();
    // Vite / webpack / esbuild emit `name-HASH.ext` (dash, single dot).
    fs::write(dir.path().join("index-D5qCqGHz.js"), "v").unwrap();
    fs::write(dir.path().join("index.html"), "<h1/>").unwrap();
    fs::write(dir.path().join("plain.js"), "y").unwrap();
    // A real hyphenated name (a word, not a hash) must stay on the normal cache.
    fs::write(dir.path().join("main-component.js"), "z").unwrap();
    let cfg = static_cfg(dir.path(), "");

    for hashed in ["/app.4f1c2a9b.js", "/index-D5qCqGHz.js"] {
        let resp = serve(Method::GET, hashed, HeaderMap::new(), &cfg).await.unwrap();
        assert!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("immutable"),
            "{hashed} should be immutable-cached"
        );
    }

    let resp = serve(Method::GET, "/index.html", HeaderMap::new(), &cfg).await.unwrap();
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap().to_str().unwrap(),
        "no-cache"
    );

    for normal in ["/plain.js", "/main-component.js"] {
        let resp = serve(Method::GET, normal, HeaderMap::new(), &cfg).await.unwrap();
        let cc = resp.headers().get(header::CACHE_CONTROL).unwrap().to_str().unwrap();
        assert!(
            cc.contains("max-age=3600") && !cc.contains("immutable"),
            "{normal} should use the normal asset cache, got {cc}"
        );
    }
}

// ───────────────────────────── SPA fallback ─────────────────────────────────

#[tokio::test]
async fn spa_fallback_serves_index_for_unknown_html_route_but_404s_missing_asset() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("index.html"), "<div id=app></div>").unwrap();
    let cfg = static_cfg(dir.path(), "spa_fallback = true\n");

    // Unknown extensionless route + Accept: text/html → index (history routing).
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
    let resp = serve(Method::GET, "/dashboard/settings", headers, &cfg)
        .await
        .expect("served");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, b"<div id=app></div>");

    // Missing asset (has an extension) → 404, NOT index.html.
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
    let resp = serve(Method::GET, "/missing.js", headers, &cfg).await.expect("owns");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn custom_not_found_file_is_served() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("index.html"), "home").unwrap();
    fs::write(dir.path().join("404.html"), "<h1>nope</h1>").unwrap();
    let cfg = static_cfg(dir.path(), "not_found = \"404.html\"\n");

    let resp = serve(Method::GET, "/does-not-exist", HeaderMap::new(), &cfg)
        .await
        .expect("owns");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_bytes(resp).await, b"<h1>nope</h1>");
}

// ───────────────────────────── the agent angle ──────────────────────────────

#[tokio::test]
async fn live_instance_serves_its_llms_txt_and_well_known() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("llms.txt"), "# riz\nWhen to use...").unwrap();
    fs::create_dir(dir.path().join(".well-known")).unwrap();
    fs::write(
        dir.path().join(".well-known/riz.json"),
        "{\"mcp\":\"/_riz/mcp\"}",
    )
    .unwrap();
    let cfg = static_cfg(dir.path(), "");

    let resp = serve(Method::GET, "/llms.txt", HeaderMap::new(), &cfg).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/plain"));
    assert!(String::from_utf8(body_bytes(resp).await).unwrap().contains("When to use"));

    let resp = serve(Method::GET, "/.well-known/riz.json", HeaderMap::new(), &cfg)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(String::from_utf8(body_bytes(resp).await).unwrap().contains("/_riz/mcp"));
}

// ───────────────────────────── mount handling ───────────────────────────────

#[tokio::test]
async fn path_outside_mount_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("index.html"), "site").unwrap();
    let cfg = static_cfg(dir.path(), "mount = \"/site\"\n");

    // Under mount → owns the response.
    assert!(serve(Method::GET, "/site/", HeaderMap::new(), &cfg).await.is_some());
    // Outside mount → None, so the caller falls through to the API 404.
    assert!(serve(Method::GET, "/api/data", HeaderMap::new(), &cfg).await.is_none());
}

// ───────────────────────────── config validation ────────────────────────────

#[test]
fn static_dir_missing_is_a_startup_error() {
    let toml = r#"
[server]
port = 0
host = "127.0.0.1"

[static]
dir = "/this/path/definitely/does/not/exist/riz"
"#;
    let config: riz::config::Config = toml::from_str(toml).expect("parses");
    let err = config.validate().expect_err("missing static dir must fail validation");
    assert!(
        err.to_string().to_lowercase().contains("static"),
        "error should mention the static dir: {err}"
    );
}

#[test]
fn static_mount_colliding_with_function_route_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[server]
port = 0
host = "127.0.0.1"

[static]
dir = {:?}
mount = "/api"

[function.data]
runtime = "bun"
handler = "x"

[[function.data.routes]]
path = "/api/data"
method = "GET"
"#,
        dir.path().display()
    );
    let config: riz::config::Config = toml::from_str(&toml).expect("parses");
    let err = config
        .validate()
        .expect_err("mount colliding with a function route must fail");
    assert!(
        err.to_string().contains("/api"),
        "collision error should name the mount: {err}"
    );
}

#[test]
fn static_mount_under_riz_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[server]\nport = 0\nhost = \"127.0.0.1\"\n\n[static]\ndir = {:?}\nmount = \"/_riz\"\n",
        dir.path().display()
    );
    let config: riz::config::Config = toml::from_str(&toml).expect("parses");
    assert!(
        config.validate().is_err(),
        "mounting static under /_riz must be rejected"
    );
}

// ───────────────────── precedence keystone (full dispatch) ───────────────────

/// A function handler that never touches a process — enough to prove the
/// router owns a path and that a static file at the same path is shadowed.
struct MockHandler {
    name: String,
    routes: Vec<riz::runtime::RouteEntry>,
}

#[async_trait::async_trait]
impl riz::runtime::LambdaHandler for MockHandler {
    fn name(&self) -> &str {
        &self.name
    }
    fn routes(&self) -> &[riz::runtime::RouteEntry] {
        &self.routes
    }
    async fn invoke(
        &self,
        _event: riz::gateway::ApiGatewayV2httpRequest,
    ) -> Result<riz::gateway::ApiGatewayV2httpResponse, riz::runtime::HandlerError> {
        Ok(riz::runtime::response::text_response(
            200,
            "text/plain",
            "FROM_FUNCTION".to_string(),
        ))
    }
}

async fn boot_with_static(config_toml: &str, handler_route: &str) -> SocketAddr {
    let config: riz::config::Config = toml::from_str(config_toml).expect("config parses");
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().expect("registry"));
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));

    let handler = Arc::new(MockHandler {
        name: "mock".to_string(),
        routes: vec![riz::runtime::RouteEntry {
            method: riz::runtime::RouteMethod::Get,
            path: handler_route.to_string(),
        }],
    }) as Arc<dyn riz::runtime::LambdaHandler>;
    let router = riz::router::Router::new(vec![handler]);

    let app_state = Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let bound = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let app =
            riz::server::build_app(app_state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.expect("axum::serve");
    });
    bound
}

async fn wait_ready(client: &reqwest::Client, url: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if client.get(url).send().await.is_ok() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "server never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn function_route_wins_over_static_file_at_same_path() {
    let dir = tempfile::tempdir().unwrap();
    // A static file sits at the exact path a function owns.
    fs::write(dir.path().join("shared"), "FROM_DISK").unwrap();
    fs::write(dir.path().join("index.html"), "<h1>home</h1>").unwrap();
    let config_toml = format!(
        "[server]\nport = 0\nhost = \"127.0.0.1\"\n\n[static]\ndir = {:?}\n",
        dir.path().display()
    );
    let addr = boot_with_static(&config_toml, "/shared").await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");
    wait_ready(&client, &format!("{base}/")).await;

    // The function owns /shared → its response wins, the disk file is shadowed.
    let resp = client.get(format!("{base}/shared")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "FROM_FUNCTION", "function must win over the static file");

    // A path NO function owns falls through to static (proves static IS wired).
    let resp = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "<h1>home</h1>");

    // A POST to the function's path is still the function's (405/handled), never
    // the static layer — static is GET/HEAD only and the function owns the path.
    let resp = client.post(format!("{base}/shared")).send().await.unwrap();
    assert_ne!(
        resp.text().await.unwrap(),
        "FROM_DISK",
        "static must never answer a POST"
    );
}
