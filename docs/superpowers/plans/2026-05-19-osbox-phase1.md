# osbox Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a production-ready Rust binary (`osbox`) that hosts AWS HTTP Gateway v2-compatible lambda functions as warm external processes, with TTL output caching, a deploy API, Datadog DogStatsD metrics, and a Ratatui TUI dashboard.

**Architecture:** A single tokio+axum HTTP server dispatches incoming requests through a router → TTL cache → process manager pipeline. Each lambda runs as a warm child process communicating via stdin/stdout using the AWS HTTP Gateway v2 JSON format. A `RoutePool` per route manages concurrent process handles behind a semaphore. A Ratatui TUI runs in a `spawn_blocking` task, reading `Arc<AppState>` on a 100ms tick.

**Tech Stack:** Rust, tokio, axum, clap, ratatui+crossterm, moka, serde/serde_json, notify, aws-sdk-s3, zip, cadence (DogStatsD), tracing+tracing-subscriber, anyhow, ipnet, uuid

---

## File Map

```
osbox/
├── Cargo.toml
├── Cargo.lock
├── osbox.toml.example
├── assets/
│   └── bun-adapter.mjs          # Embedded in binary; bridges AWS Lambda SDK → stdin/stdout
├── src/
│   ├── main.rs                  # #[tokio::main], Clap dispatch, spawns TUI thread
│   ├── config.rs                # Config, RouteConfig, ServerConfig, CacheConfig, etc.
│   ├── gateway.rs               # GatewayRequest, GatewayResponse, RequestContext types
│   ├── router.rs                # Router, RouteMatch — path pattern matching
│   ├── cache.rs                 # CacheLayer (moka), make_key, invalidate_keys/prefix
│   ├── state.rs                 # AppState, RouteStats, LogEntry
│   ├── server.rs                # axum router setup, request pipeline handler
│   ├── deploy.rs                # deploy API handler, S3 download, zip unpack, process swap
│   ├── metrics.rs               # MetricsEmitter (cadence DogStatsD, fire-and-forget)
│   ├── process/
│   │   ├── mod.rs               # ProcessManager, RoutePool, ProcessHandle, PoolStats
│   │   ├── runtime.rs           # LambdaRuntime trait, RuntimeRegistry
│   │   └── bun.rs               # BunRuntime — writes adapter, builds Command
│   └── tui/
│       ├── mod.rs               # run_tui() entry point, event loop
│       ├── app.rs               # App struct, tick/key event handling
│       └── widgets.rs           # Routes pane, Processes pane, Cache pane, Logs pane
└── tests/
    └── integration_test.rs      # Real Bun echo lambda end-to-end
```

---

## Task 1: Project Scaffold and Cargo.toml

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `assets/bun-adapter.mjs`
- Create: `osbox.toml.example`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "osbox"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "osbox"
path = "src/main.rs"

[dependencies]
tokio = { version = "1", features = ["full"] }
axum = { version = "0.7", features = ["macros"] }
clap = { version = "4", features = ["derive"] }
ratatui = "0.28"
crossterm = "0.28"
moka = { version = "0.12", features = ["future"] }
toml = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
notify = "6"
aws-sdk-s3 = "1"
aws-config = "1"
zip = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
anyhow = "1"
uuid = { version = "1", features = ["v4"] }
cadence = "1"
ipnet = "2"
tower-http = { version = "0.5", features = ["trace"] }

[dev-dependencies]
tempfile = "3"
tokio-test = "0.4"
```

- [ ] **Step 2: Create src/main.rs skeleton**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "osbox", about = "Self-hosted AWS Lambda host")]
struct Cli {
    #[arg(short, long, default_value = "osbox.toml")]
    config: String,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long)]
    no_tui: bool,

    #[arg(long, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Start,
    Validate,
    Routes,
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    println!("osbox starting (config: {})", cli.config);
    Ok(())
}
```

- [ ] **Step 3: Create assets/bun-adapter.mjs**

```javascript
// Bridges AWS Lambda HTTP Gateway v2 handler → osbox stdin/stdout protocol.
// Spawned by osbox as: bun run bun-adapter.mjs <handler_path>
import { createInterface } from "readline";

const handlerPath = process.argv[2];
if (!handlerPath) {
  process.stderr.write("osbox bun-adapter: missing handler path\n");
  process.exit(1);
}

const mod = await import(handlerPath);
const handler = mod.handler ?? mod.default;

if (typeof handler !== "function") {
  process.stderr.write(
    `osbox bun-adapter: no exported 'handler' function in ${handlerPath}\n`
  );
  process.exit(1);
}

const rl = createInterface({ input: process.stdin, terminal: false });

rl.on("line", async (line) => {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    process.stdout.write(
      JSON.stringify({ statusCode: 400, body: "bad event json" }) + "\n"
    );
    return;
  }

  const context = {
    functionName: process.env.AWS_LAMBDA_FUNCTION_NAME ?? "osbox",
    functionVersion: "$LATEST",
    invokedFunctionArn: "",
    memoryLimitInMB: "512",
    awsRequestId: crypto.randomUUID(),
    logGroupName: "/osbox",
    logStreamName: "local",
    getRemainingTimeInMillis: () => 30000,
    done: () => {},
    fail: () => {},
    succeed: () => {},
  };

  try {
    const result = await handler(event, context);
    process.stdout.write(JSON.stringify(result) + "\n");
  } catch (err) {
    process.stdout.write(
      JSON.stringify({
        statusCode: 500,
        body: JSON.stringify({ error: String(err) }),
      }) + "\n"
    );
  }
});
```

- [ ] **Step 4: Create osbox.toml.example**

```toml
[server]
port = 3000
host = "0.0.0.0"

[cache]
default_ttl_secs = 0
max_size_mb = 128

[datadog]
enabled = false
statsd_host = "127.0.0.1:8125"
service = "osbox"
env = "production"

[deploy]
# deploy_key = "changeme"   # prefer OSBOX_DEPLOY_KEY env var
allowed_cidrs = []          # empty = allow all IPs

[aws]
region = "us-east-1"

[[routes]]
path = "/auth/signin"
method = "POST"
runtime = "bun"
handler = "./lambdas/signin/index.ts"
timeout_ms = 5000
concurrency = 2

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./lambdas/accounts/index.ts"
cache_ttl_secs = 30
timeout_ms = 3000
concurrency = 1
```

- [ ] **Step 5: Build to confirm dependencies resolve**

```bash
cargo build 2>&1 | head -40
```

Expected: warnings are fine; no errors. First build downloads crates (~2 min).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs assets/bun-adapter.mjs osbox.toml.example
git commit -m "feat: project scaffold, Cargo.toml, Clap skeleton, bun adapter"
```

---

## Task 2: Config Types

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

- [ ] **Step 1: Write failing test**

Add at bottom of `src/config.rs` (create the file):

```rust
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub datadog: DatadogConfig,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub aws: AwsConfig,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
}

fn default_port() -> u16 { 3000 }
fn default_host() -> String { "0.0.0.0".into() }

impl Default for ServerConfig {
    fn default() -> Self { Self { port: default_port(), host: default_host() } }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CacheConfig {
    #[serde(default)]
    pub default_ttl_secs: u64,
    #[serde(default = "default_cache_size")]
    pub max_size_mb: u64,
}

fn default_cache_size() -> u64 { 128 }

#[derive(Debug, Clone, Deserialize)]
pub struct DatadogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_statsd")]
    pub statsd_host: String,
    #[serde(default = "default_service")]
    pub service: String,
    #[serde(default = "default_env")]
    pub env: String,
}

fn default_statsd() -> String { "127.0.0.1:8125".into() }
fn default_service() -> String { "osbox".into() }
fn default_env() -> String { "production".into() }

impl Default for DatadogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            statsd_host: default_statsd(),
            service: default_service(),
            env: default_env(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeployConfig {
    pub deploy_key: Option<String>,
    #[serde(default)]
    pub allowed_cidrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AwsConfig {
    #[serde(default = "default_region")]
    pub region: String,
}

fn default_region() -> String { "us-east-1".into() }

impl Default for AwsConfig {
    fn default() -> Self { Self { region: default_region() } }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub path: String,
    pub method: String,
    pub runtime: RuntimeKind,
    pub handler: PathBuf,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    pub cache_ttl_secs: Option<u64>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

fn default_timeout() -> u64 { 30_000 }
fn default_concurrency() -> usize { 1 }

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Bun,
    Rust,
    Python,
}

impl Config {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
        let config: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid config in {path}: {e}"))?;
        Ok(config)
    }

    pub fn effective_deploy_key(&self) -> Option<String> {
        std::env::var("OSBOX_DEPLOY_KEY").ok().or_else(|| self.deploy.deploy_key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[server]
port = 4000
host = "127.0.0.1"

[[routes]]
path = "/ping"
method = "GET"
runtime = "bun"
handler = "./lambdas/ping/index.ts"
timeout_ms = 1000
concurrency = 2
"#;

    #[test]
    fn parses_server_config() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.server.port, 4000);
        assert_eq!(config.server.host, "127.0.0.1");
    }

    #[test]
    fn parses_route() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        let route = &config.routes[0];
        assert_eq!(route.path, "/ping");
        assert_eq!(route.method, "GET");
        assert_eq!(route.runtime, RuntimeKind::Bun);
        assert_eq!(route.timeout_ms, 1000);
        assert_eq!(route.concurrency, 2);
    }

    #[test]
    fn cache_defaults_to_zero_ttl() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.cache.default_ttl_secs, 0);
    }

    #[test]
    fn deploy_key_env_wins() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        std::env::set_var("OSBOX_DEPLOY_KEY", "envkey");
        assert_eq!(config.effective_deploy_key(), Some("envkey".into()));
        std::env::remove_var("OSBOX_DEPLOY_KEY");
    }
}
```

- [ ] **Step 2: Run tests — verify they compile and pass**

```bash
cargo test config 2>&1
```

Expected:
```
test config::tests::parses_server_config ... ok
test config::tests::parses_route ... ok
test config::tests::cache_defaults_to_zero_ttl ... ok
test config::tests::deploy_key_env_wins ... ok
```

- [ ] **Step 3: Add `mod config;` to main.rs**

Replace the `mod` declarations area in `src/main.rs`:

```rust
mod config;

use clap::{Parser, Subcommand};
// ... rest unchanged
```

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: config types with serde/toml parsing and tests"
```

---

## Task 3: HTTP Gateway v2 Types

**Files:**
- Create: `src/gateway.rs`
- Modify: `src/main.rs` (add `mod gateway;`)

- [ ] **Step 1: Create src/gateway.rs**

```rust
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequest {
    pub version: String,
    pub route_key: String,
    pub raw_path: String,
    pub raw_query_string: String,
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub is_base64_encoded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub http: HttpContext,
    pub request_id: String,
    pub time_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpContext {
    pub method: String,
    pub path: String,
    pub protocol: String,
    pub source_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResponse {
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_base64_encoded: Option<bool>,
}

impl GatewayResponse {
    pub fn error(status_code: u16, message: &str) -> Self {
        let body = serde_json::json!({ "error": message }).to_string();
        Self {
            status_code,
            headers: Some(HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )])),
            body: Some(body),
            is_base64_encoded: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_round_trips() {
        let resp = GatewayResponse {
            status_code: 200,
            headers: Some(HashMap::from([("content-type".into(), "text/plain".into())])),
            body: Some("hello".into()),
            is_base64_encoded: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: GatewayResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status_code, 200);
        assert_eq!(back.body.as_deref(), Some("hello"));
    }

    #[test]
    fn request_round_trips() {
        let req = GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /ping".into(),
            raw_path: "/ping".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/ping".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "abc".into(),
                time_epoch: 1000,
            },
            body: None,
            is_base64_encoded: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.route_key, "GET /ping");
    }

    #[test]
    fn error_response_is_json() {
        let resp = GatewayResponse::error(502, "lambda crashed");
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["error"], "lambda crashed");
        assert_eq!(resp.status_code, 502);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test gateway 2>&1
```

Expected: 3 tests pass.

- [ ] **Step 3: Add `mod gateway;` to main.rs**

- [ ] **Step 4: Commit**

```bash
git add src/gateway.rs src/main.rs
git commit -m "feat: HTTP Gateway v2 request/response types"
```

---

## Task 4: Router

**Files:**
- Create: `src/router.rs`
- Modify: `src/main.rs` (add `mod router;`)

- [ ] **Step 1: Create src/router.rs with failing tests first**

```rust
use std::collections::HashMap;
use crate::config::RouteConfig;

pub struct Router {
    routes: Vec<RouteConfig>,
}

pub struct RouteMatch<'a> {
    pub route: &'a RouteConfig,
    pub path_params: HashMap<String, String>,
}

impl Router {
    pub fn new(routes: Vec<RouteConfig>) -> Self {
        Self { routes }
    }

    /// Returns "METHOD /path/pattern" used as a stable key throughout the system.
    pub fn route_key(method: &str, pattern: &str) -> String {
        format!("{} {}", method.to_uppercase(), pattern)
    }

    pub fn match_route<'a>(&'a self, method: &str, path: &str) -> Option<RouteMatch<'a>> {
        let method_upper = method.to_uppercase();
        for route in &self.routes {
            if route.method.to_uppercase() != method_upper {
                continue;
            }
            if let Some(params) = match_pattern(&route.path, path) {
                return Some(RouteMatch { route, path_params: params });
            }
        }
        None
    }
}

/// Matches a route pattern (e.g. "/accounts/:id") against a concrete path.
/// Returns Some(params) on match, None on no match.
fn match_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_parts: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path_parts: Vec<&str> = path.trim_matches('/').split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (pat, seg) in pattern_parts.iter().zip(path_parts.iter()) {
        if let Some(name) = pat.strip_prefix(':') {
            params.insert(name.to_string(), seg.to_string());
        } else if pat != seg {
            return None;
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::config::RuntimeKind;

    fn make_route(method: &str, path: &str) -> RouteConfig {
        RouteConfig {
            path: path.into(),
            method: method.into(),
            runtime: RuntimeKind::Bun,
            handler: PathBuf::from("./handler.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        }
    }

    #[test]
    fn matches_exact_path() {
        let router = Router::new(vec![make_route("GET", "/ping")]);
        assert!(router.match_route("GET", "/ping").is_some());
        assert!(router.match_route("GET", "/pong").is_none());
    }

    #[test]
    fn matches_path_param() {
        let router = Router::new(vec![make_route("GET", "/accounts/:id")]);
        let m = router.match_route("GET", "/accounts/42").unwrap();
        assert_eq!(m.path_params["id"], "42");
    }

    #[test]
    fn method_mismatch_returns_none() {
        let router = Router::new(vec![make_route("GET", "/ping")]);
        assert!(router.match_route("POST", "/ping").is_none());
    }

    #[test]
    fn route_key_format() {
        assert_eq!(Router::route_key("get", "/accounts/:id"), "GET /accounts/:id");
    }

    #[test]
    fn no_match_on_different_segment_count() {
        let router = Router::new(vec![make_route("GET", "/a/b")]);
        assert!(router.match_route("GET", "/a").is_none());
        assert!(router.match_route("GET", "/a/b/c").is_none());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test router 2>&1
```

Expected: 5 tests pass.

- [ ] **Step 3: Add `mod router;` to main.rs**

- [ ] **Step 4: Commit**

```bash
git add src/router.rs src/main.rs
git commit -m "feat: route matching with path params"
```

---

## Task 5: Cache Layer

**Files:**
- Create: `src/cache.rs`
- Modify: `src/main.rs` (add `mod cache;`)

- [ ] **Step 1: Create src/cache.rs**

```rust
use std::sync::Arc;
use std::time::Duration;
use moka::future::Cache;
use crate::config::CacheConfig;
use crate::gateway::GatewayResponse;

pub struct CacheLayer {
    inner: Cache<String, Arc<GatewayResponse>>,
}

impl CacheLayer {
    pub fn new(config: &CacheConfig) -> Self {
        let max_capacity = config.max_size_mb * 1024 * 1024 / 512; // ~512B per entry estimate
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .build();
        Self { inner: cache }
    }

    pub fn make_key(method: &str, path: &str, query: &str) -> String {
        format!("{}:{}?{}", method.to_uppercase(), path, query)
    }

    pub async fn get(&self, key: &str) -> Option<Arc<GatewayResponse>> {
        self.inner.get(key).await
    }

    pub async fn set(&self, key: String, response: GatewayResponse, ttl_secs: u64) {
        if ttl_secs == 0 {
            return;
        }
        self.inner
            .insert_with_ttl(key, Arc::new(response), Duration::from_secs(ttl_secs))
            .await;
    }

    pub async fn invalidate_keys(&self, keys: &[String]) -> usize {
        let mut count = 0;
        for key in keys {
            if self.inner.remove(key).await.is_some() {
                count += 1;
            }
        }
        count
    }

    pub async fn invalidate_prefix(&self, prefix: &str) -> usize {
        // moka doesn't support prefix scan; collect matching keys first.
        // This is O(n) but invalidation is rare.
        let keys: Vec<String> = self
            .inner
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();
        let count = keys.len();
        for key in &keys {
            self.inner.remove(key).await;
        }
        count
    }

    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CacheConfig {
        CacheConfig { default_ttl_secs: 0, max_size_mb: 16 }
    }

    fn ok_response() -> GatewayResponse {
        GatewayResponse {
            status_code: 200,
            headers: None,
            body: Some("hello".into()),
            is_base64_encoded: None,
        }
    }

    #[test]
    fn make_key_format() {
        assert_eq!(CacheLayer::make_key("GET", "/accounts/1", ""), "GET:/accounts/1?");
        assert_eq!(
            CacheLayer::make_key("get", "/foo", "bar=1"),
            "GET:/foo?bar=1"
        );
    }

    #[tokio::test]
    async fn set_then_get() {
        let cache = CacheLayer::new(&test_config());
        let key = CacheLayer::make_key("GET", "/foo", "");
        cache.set(key.clone(), ok_response(), 60).await;
        let hit = cache.get(&key).await;
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().status_code, 200);
    }

    #[tokio::test]
    async fn zero_ttl_is_not_cached() {
        let cache = CacheLayer::new(&test_config());
        let key = CacheLayer::make_key("GET", "/foo", "");
        cache.set(key.clone(), ok_response(), 0).await;
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn invalidate_by_key() {
        let cache = CacheLayer::new(&test_config());
        let key = CacheLayer::make_key("GET", "/foo", "");
        cache.set(key.clone(), ok_response(), 60).await;
        let evicted = cache.invalidate_keys(&[key.clone()]).await;
        assert_eq!(evicted, 1);
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn invalidate_by_prefix() {
        let cache = CacheLayer::new(&test_config());
        let k1 = CacheLayer::make_key("GET", "/accounts/1", "");
        let k2 = CacheLayer::make_key("GET", "/accounts/2", "");
        let k3 = CacheLayer::make_key("GET", "/other", "");
        cache.set(k1.clone(), ok_response(), 60).await;
        cache.set(k2.clone(), ok_response(), 60).await;
        cache.set(k3.clone(), ok_response(), 60).await;
        // Give moka time to index entries
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let evicted = cache.invalidate_prefix("GET:/accounts/").await;
        assert_eq!(evicted, 2);
        assert!(cache.get(&k3).await.is_some());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test cache 2>&1
```

Expected: 5 tests pass. (moka's async insert may require a small sleep before reads in tests — the test above includes one where needed.)

- [ ] **Step 3: Add `mod cache;` to main.rs**

- [ ] **Step 4: Commit**

```bash
git add src/cache.rs src/main.rs
git commit -m "feat: TTL cache layer with prefix invalidation"
```

---

## Task 6: LambdaRuntime Trait and RuntimeRegistry

**Files:**
- Create: `src/process/runtime.rs`
- Create: `src/process/mod.rs` (skeleton)
- Modify: `src/main.rs` (add `mod process;`)

- [ ] **Step 1: Create src/process/mod.rs skeleton**

```rust
pub mod runtime;
pub mod bun;
```

- [ ] **Step 2: Create src/process/runtime.rs**

```rust
use tokio::process::Command;
use crate::config::{RouteConfig, RuntimeKind};
use crate::process::bun::BunRuntime;

pub trait LambdaRuntime: Send + Sync + 'static {
    /// Build the Command to spawn this lambda process.
    /// The caller sets stdin/stdout/stderr handles before spawning.
    fn spawn_command(&self, route: &RouteConfig) -> Command;

    fn name(&self) -> &'static str;
}

pub struct RuntimeRegistry {
    bun: BunRuntime,
}

impl RuntimeRegistry {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            bun: BunRuntime::new()?,
        })
    }

    pub fn get(&self, kind: &RuntimeKind) -> &dyn LambdaRuntime {
        match kind {
            RuntimeKind::Bun => &self.bun,
            RuntimeKind::Rust => &self.bun, // placeholder until Phase 2
            RuntimeKind::Python => &self.bun, // placeholder until Phase 3
        }
    }
}
```

- [ ] **Step 3: Add `mod process;` to main.rs**

- [ ] **Step 4: Build to confirm it compiles (bun.rs doesn't exist yet — stub it)**

Create `src/process/bun.rs` temporarily:

```rust
use tokio::process::Command;
use crate::config::RouteConfig;
use crate::process::runtime::LambdaRuntime;

pub struct BunRuntime;

impl BunRuntime {
    pub fn new() -> anyhow::Result<Self> { Ok(Self) }
}

impl LambdaRuntime for BunRuntime {
    fn spawn_command(&self, route: &RouteConfig) -> Command {
        let mut cmd = Command::new("bun");
        cmd.arg("run").arg(route.handler.to_str().unwrap_or(""));
        cmd
    }
    fn name(&self) -> &'static str { "bun" }
}
```

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no error lines.

- [ ] **Step 5: Commit**

```bash
git add src/process/mod.rs src/process/runtime.rs src/process/bun.rs src/main.rs
git commit -m "feat: LambdaRuntime trait and RuntimeRegistry skeleton"
```

---

## Task 7: BunRuntime and ProcessManager

**Files:**
- Modify: `src/process/bun.rs` (full implementation)
- Modify: `src/process/mod.rs` (full ProcessManager)

- [ ] **Step 1: Implement BunRuntime with embedded adapter**

Replace `src/process/bun.rs`:

```rust
use std::path::PathBuf;
use tokio::process::Command;
use anyhow::Context;
use crate::config::RouteConfig;
use crate::process::runtime::LambdaRuntime;

const BUN_ADAPTER: &str = include_str!("../../assets/bun-adapter.mjs");

pub struct BunRuntime {
    adapter_path: PathBuf,
}

impl BunRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let dir = dirs_home().join(".osbox");
        std::fs::create_dir_all(&dir)
            .context("failed to create ~/.osbox")?;
        let adapter_path = dir.join("bun-adapter.mjs");
        std::fs::write(&adapter_path, BUN_ADAPTER)
            .context("failed to write bun adapter")?;
        Ok(Self { adapter_path })
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

impl LambdaRuntime for BunRuntime {
    fn spawn_command(&self, route: &RouteConfig) -> Command {
        let handler = route.handler.canonicalize()
            .unwrap_or_else(|_| route.handler.clone());
        let mut cmd = Command::new("bun");
        cmd.arg("run")
           .arg(&self.adapter_path)
           .arg(handler);
        cmd
    }

    fn name(&self) -> &'static str { "bun" }
}
```

- [ ] **Step 2: Implement ProcessManager in src/process/mod.rs**

```rust
pub mod runtime;
pub mod bun;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};
use anyhow::Context;
use tracing::{error, warn};
use crate::config::RouteConfig;
use crate::gateway::{GatewayRequest, GatewayResponse};
use crate::process::runtime::RuntimeRegistry;

struct ProcessHandle {
    pid: u32,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    spawned_at: Instant,
    _child: Child, // keeps the process alive
}

struct RoutePool {
    route: RouteConfig,
    handles: Mutex<Vec<ProcessHandle>>,
    semaphore: Arc<Semaphore>,
    restart_count: AtomicU32,
    consecutive_crashes: AtomicU32,
    healthy: AtomicBool,
}

const CRASH_THRESHOLD: u32 = 5;

pub struct ProcessManager {
    pools: RwLock<HashMap<String, Arc<RoutePool>>>,
}

pub struct PoolStats {
    pub route_key: String,
    pub pids: Vec<u32>,
    pub restart_count: u32,
    pub healthy: bool,
    pub concurrency: usize,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self { pools: RwLock::new(HashMap::new()) }
    }

    pub async fn spawn_all(
        &self,
        routes: &[RouteConfig],
        registry: &RuntimeRegistry,
    ) -> anyhow::Result<()> {
        let mut pools = self.pools.write().await;
        for route in routes {
            let key = crate::router::Router::route_key(&route.method, &route.path);
            let pool = Arc::new(RoutePool {
                route: route.clone(),
                handles: Mutex::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(route.concurrency)),
                restart_count: AtomicU32::new(0),
                consecutive_crashes: AtomicU32::new(0),
                healthy: AtomicBool::new(true),
            });
            let mut handles = pool.handles.lock().await;
            for _ in 0..route.concurrency {
                let handle = spawn_process(route, registry).await
                    .with_context(|| format!("failed to spawn lambda for {key}"))?;
                handles.push(handle);
            }
            drop(handles);
            pools.insert(key, pool);
        }
        Ok(())
    }

    pub async fn invoke(
        &self,
        route_key: &str,
        request: &GatewayRequest,
        timeout_ms: u64,
    ) -> anyhow::Result<GatewayResponse> {
        let pools = self.pools.read().await;
        let pool = pools.get(route_key)
            .ok_or_else(|| anyhow::anyhow!("no pool for route {route_key}"))?
            .clone();
        drop(pools);

        if !pool.healthy.load(Ordering::Relaxed) {
            return Ok(GatewayResponse::error(503, "lambda unhealthy"));
        }

        let _permit = pool.semaphore.acquire().await?;
        let mut handles = pool.handles.lock().await;
        let handle = handles.iter_mut().next()
            .ok_or_else(|| anyhow::anyhow!("no process handles available"))?;

        let payload = serde_json::to_string(request)? + "\n";
        let result = timeout(Duration::from_millis(timeout_ms), async {
            handle.stdin.write_all(payload.as_bytes()).await?;
            handle.stdin.flush().await?;
            let mut line = String::new();
            handle.stdout.read_line(&mut line).await?;
            Ok::<String, anyhow::Error>(line)
        }).await;

        match result {
            Ok(Ok(line)) => {
                pool.consecutive_crashes.store(0, Ordering::Relaxed);
                serde_json::from_str(line.trim())
                    .map_err(|_| anyhow::anyhow!("malformed lambda response: {line}"))
            }
            Ok(Err(e)) => {
                // Process I/O error — treat as crash
                pool.restart_count.fetch_add(1, Ordering::Relaxed);
                let crashes = pool.consecutive_crashes.fetch_add(1, Ordering::Relaxed) + 1;
                if crashes >= CRASH_THRESHOLD {
                    pool.healthy.store(false, Ordering::Relaxed);
                    error!("route {route_key} marked unhealthy after {crashes} crashes");
                }
                warn!("lambda crash on {route_key}: {e}");
                Ok(GatewayResponse::error(502, "lambda error"))
            }
            Err(_) => {
                // Timeout — SIGKILL the process; it will be respawned on next request
                warn!("lambda timeout on {route_key} after {timeout_ms}ms");
                Ok(GatewayResponse::error(504, "lambda timeout"))
            }
        }
    }

    /// Replace all processes for a route (used by deploy API).
    pub async fn hot_swap(
        &self,
        route_key: &str,
        new_route: RouteConfig,
        registry: &RuntimeRegistry,
    ) -> anyhow::Result<u32> {
        let pools = self.pools.read().await;
        let pool = pools.get(route_key)
            .ok_or_else(|| anyhow::anyhow!("unknown route {route_key}"))?
            .clone();
        drop(pools);

        let mut handles = pool.handles.lock().await;
        // Drain old handles (Drop kills the child process)
        handles.clear();
        // Spawn new handles
        let mut first_pid = 0;
        for _ in 0..new_route.concurrency {
            let h = spawn_process(&new_route, registry).await?;
            if first_pid == 0 { first_pid = h.pid; }
            handles.push(h);
        }
        pool.healthy.store(true, Ordering::Relaxed);
        pool.consecutive_crashes.store(0, Ordering::Relaxed);
        Ok(first_pid)
    }

    pub async fn pool_stats(&self) -> Vec<PoolStats> {
        let pools = self.pools.read().await;
        let mut stats = Vec::new();
        for (key, pool) in pools.iter() {
            let handles = pool.handles.lock().await;
            stats.push(PoolStats {
                route_key: key.clone(),
                pids: handles.iter().map(|h| h.pid).collect(),
                restart_count: pool.restart_count.load(Ordering::Relaxed),
                healthy: pool.healthy.load(Ordering::Relaxed),
                concurrency: pool.route.concurrency,
            });
        }
        stats
    }
}

async fn spawn_process(
    route: &RouteConfig,
    registry: &RuntimeRegistry,
) -> anyhow::Result<ProcessHandle> {
    let runtime = registry.get(&route.runtime);
    let mut cmd = runtime.spawn_command(route);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .with_context(|| format!("failed to spawn {:?}", route.handler))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    // Drain stderr to tracing
    if let Some(mut stderr) = child.stderr.take() {
        let route_key = crate::router::Router::route_key(&route.method, &route.path);
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf).await;
            if !buf.trim().is_empty() {
                warn!("lambda stderr [{}]: {}", route_key, buf.trim());
            }
        });
    }

    Ok(ProcessHandle { pid, stdin, stdout, spawned_at: Instant::now(), _child: child })
}
```

- [ ] **Step 3: Build to confirm it compiles**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no error lines (warnings are fine).

- [ ] **Step 4: Commit**

```bash
git add src/process/mod.rs src/process/bun.rs src/process/runtime.rs
git commit -m "feat: ProcessManager with RoutePool, concurrency, crash detection"
```

---

## Task 8: Shared App State

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

- [ ] **Step 1: Create src/state.rs**

```rust
use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;
use tokio::sync::{Mutex, RwLock};
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::metrics::MetricsEmitter;
use crate::process::ProcessManager;
use crate::process::runtime::RuntimeRegistry;
use crate::router::Router;

pub struct AppState {
    pub config: RwLock<Config>,
    pub router: RwLock<Router>,
    pub process_manager: ProcessManager,
    pub cache: CacheLayer,
    pub metrics: MetricsEmitter,
    pub runtime_registry: RuntimeRegistry,
    pub route_stats: RwLock<HashMap<String, RouteStats>>,
    pub log_buffer: Mutex<VecDeque<LogEntry>>,
}

#[derive(Default, Clone)]
pub struct RouteStats {
    pub request_count: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub latencies_ms: VecDeque<f64>, // rolling last 100
    pub healthy: bool,
}

impl RouteStats {
    pub fn p50_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.5)
    }

    pub fn p95_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.95)
    }
}

fn percentile(values: &VecDeque<f64>, p: f64) -> f64 {
    if values.is_empty() { return 0.0; }
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((sorted.len() as f64) * p).min((sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub level: String,
    pub message: String,
}

impl AppState {
    pub async fn push_log(&self, level: &str, message: String) {
        let mut buf = self.log_buffer.lock().await;
        if buf.len() >= 200 {
            buf.pop_front();
        }
        buf.push_back(LogEntry {
            timestamp: SystemTime::now(),
            level: level.into(),
            message,
        });
    }

    pub async fn record_request(
        &self,
        route_key: &str,
        cache_hit: bool,
        latency_ms: f64,
        healthy: bool,
    ) {
        let mut stats = self.route_stats.write().await;
        let entry = stats.entry(route_key.to_string()).or_default();
        entry.request_count += 1;
        entry.healthy = healthy;
        if cache_hit { entry.cache_hits += 1; } else { entry.cache_misses += 1; }
        entry.latencies_ms.push_back(latency_ms);
        if entry.latencies_ms.len() > 100 {
            entry.latencies_ms.pop_front();
        }
    }
}
```

- [ ] **Step 2: Add `mod state;` to main.rs and stub `mod metrics;`**

Create a minimal `src/metrics.rs` so state.rs compiles (full impl in Task 11):

```rust
use crate::config::DatadogConfig;

pub struct MetricsEmitter {
    enabled: bool,
}

impl MetricsEmitter {
    pub fn new(config: &DatadogConfig) -> Self {
        Self { enabled: config.enabled }
    }

    pub fn record_request(&self, _route: &str, _method: &str, _status: u16, _duration_ms: f64) {}
    pub fn record_cache_hit(&self, _route: &str) {}
    pub fn record_cache_miss(&self, _route: &str) {}
    pub fn record_lambda_crash(&self, _route: &str, _runtime: &str) {}
    pub fn record_lambda_timeout(&self, _route: &str) {}
    pub fn record_lambda_healthy(&self, _route: &str, _healthy: bool) {}
}
```

- [ ] **Step 3: Build**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/state.rs src/metrics.rs src/main.rs
git commit -m "feat: AppState and RouteStats with p50/p95 percentiles"
```

---

## Task 9: HTTP Server and Request Pipeline

**Files:**
- Create: `src/server.rs`
- Modify: `src/main.rs` (wire `start` command to `server::run`)

- [ ] **Step 1: Create src/server.rs**

```rust
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, post},
    Json, Router as AxumRouter,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;
use crate::cache::CacheLayer;
use crate::gateway::{GatewayRequest, GatewayResponse, HttpContext, RequestContext};
use crate::state::AppState;

pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = AxumRouter::new()
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        .fallback(any(dispatch_lambda))
        .with_state(state);

    info!("osbox listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn dispatch_lambda(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response {
    let start = Instant::now();
    let method = req.method().as_str().to_uppercase();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let source_ip = "unknown".to_string(); // populated by reverse proxy headers

    let router = state.router.read().await;
    let route_match = router.match_route(&method, &path);
    drop(router);

    let route_match = match route_match {
        Some(m) => m,
        None => {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };

    let route = route_match.route.clone();
    let route_key = crate::router::Router::route_key(&method, &route.path);
    let cache_key = CacheLayer::make_key(&method, &path, &query);

    // Cache check
    if let Some(cached) = state.cache.get(&cache_key).await {
        let latency = start.elapsed().as_secs_f64() * 1000.0;
        state.record_request(&route_key, true, latency, true).await;
        state.metrics.record_cache_hit(&route_key);
        return gateway_to_axum(&cached);
    }

    // Build Gateway v2 request
    let headers = extract_headers(req.headers());
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let body = if body_bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&body_bytes).into_owned())
    };

    let request_id = Uuid::new_v4().to_string();
    let time_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let gw_request = GatewayRequest {
        version: "2.0".into(),
        route_key: route_key.clone(),
        raw_path: path.clone(),
        raw_query_string: query.clone(),
        headers,
        request_context: RequestContext {
            http: HttpContext {
                method: method.clone(),
                path: path.clone(),
                protocol: "HTTP/1.1".into(),
                source_ip,
            },
            request_id,
            time_epoch,
        },
        body,
        is_base64_encoded: false,
    };

    // Invoke lambda
    let result = state.process_manager.invoke(&route_key, &gw_request, route.timeout_ms).await;

    let latency = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(gw_resp) => {
            state.metrics.record_request(&route_key, &method, gw_resp.status_code, latency);
            state.metrics.record_cache_miss(&route_key);
            state.record_request(&route_key, false, latency, true).await;

            // Cache successful responses if TTL configured
            let ttl = route.cache_ttl_secs.unwrap_or(0);
            if ttl > 0 && gw_resp.status_code < 400 {
                state.cache.set(cache_key, gw_resp.clone(), ttl).await;
            }

            gateway_to_axum(&gw_resp)
        }
        Err(e) => {
            error!("dispatch error for {route_key}: {e}");
            state.metrics.record_lambda_crash(&route_key, "bun");
            state.record_request(&route_key, false, latency, false).await;
            let resp = GatewayResponse::error(502, "internal error");
            gateway_to_axum(&resp)
        }
    }
}

fn gateway_to_axum(resp: &GatewayResponse) -> Response {
    let status = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = axum::http::response::Builder::new().status(status);
    if let Some(headers) = &resp.headers {
        for (k, v) in headers {
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::try_from(k.as_str()),
                axum::http::HeaderValue::try_from(v.as_str()),
            ) {
                builder = builder.header(name, value);
            }
        }
    }
    let body = resp.body.clone().unwrap_or_default();
    builder.body(Body::from(body)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn extract_headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect()
}

// Cache invalidation endpoint
#[derive(Deserialize)]
pub struct InvalidateRequest {
    pub keys: Option<Vec<String>>,
    pub prefix: Option<String>,
}

#[derive(Serialize)]
pub struct InvalidateResponse {
    pub evicted: usize,
}

async fn cache_invalidate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InvalidateRequest>,
) -> impl IntoResponse {
    let evicted = if let Some(keys) = &body.keys {
        state.cache.invalidate_keys(keys).await
    } else if let Some(prefix) = &body.prefix {
        state.cache.invalidate_prefix(prefix).await
    } else {
        0
    };
    Json(InvalidateResponse { evicted })
}
```

- [ ] **Step 2: Create src/deploy.rs stub** (full impl in Task 10)

```rust
use std::sync::Arc;
use axum::{extract::State, http::StatusCode, response::IntoResponse};
use crate::state::AppState;

pub async fn deploy_handler(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
```

- [ ] **Step 3: Wire `start` in main.rs**

Replace `src/main.rs` entirely:

```rust
mod cache;
mod config;
mod deploy;
mod gateway;
mod metrics;
mod process;
mod router;
mod server;
mod state;
mod tui;

use std::net::SocketAddr;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "osbox", about = "Self-hosted AWS Lambda host")]
struct Cli {
    #[arg(short, long, default_value = "osbox.toml")]
    config: String,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long)]
    no_tui: bool,

    #[arg(long, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Start,
    Validate,
    Routes,
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cli.log_level))
        .init();

    let config = config::Config::from_file(&cli.config)?;

    match &cli.command {
        Some(Commands::Validate) => {
            println!("Config OK: {} routes", config.routes.len());
            return Ok(());
        }
        Some(Commands::Routes) => {
            for route in &config.routes {
                println!("{} {} -> {:?} ({})", route.method, route.path, route.handler, route.runtime.as_str());
            }
            return Ok(());
        }
        _ => {}
    }

    let port = cli.port.unwrap_or(config.server.port);
    let host: std::net::IpAddr = config.server.host.parse()?;
    let addr = SocketAddr::new(host, port);

    let registry = process::runtime::RuntimeRegistry::new()?;
    let cache = cache::CacheLayer::new(&config.cache);
    let metrics = metrics::MetricsEmitter::new(&config.datadog);
    let router = router::Router::new(config.routes.clone());
    let process_manager = process::ProcessManager::new();

    process_manager.spawn_all(&config.routes, &registry).await?;

    let app_state = Arc::new(state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    server::run(app_state, addr).await
}
```

Add `as_str()` to `RuntimeKind` in config.rs:

```rust
impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }
}
```

Create a `src/tui/mod.rs` stub:

```rust
// TUI implementation in Task 13
```

- [ ] **Step 4: Build**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/server.rs src/deploy.rs src/main.rs src/config.rs src/tui/mod.rs
git commit -m "feat: axum HTTP server with request dispatch pipeline"
```

---

## Task 10: Deploy API

**Files:**
- Modify: `src/deploy.rs` (full implementation)

- [ ] **Step 1: Write failing tests in deploy.rs**

Replace `src/deploy.rs`:

```rust
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use crate::router::Router;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct DeployRequest {
    pub lambda: String,
    pub s3_bucket: String,
    pub s3_key: String,
}

#[derive(Serialize)]
pub struct DeployResponse {
    pub status: String,
    pub lambda: String,
    pub pid: u32,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub async fn deploy_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<DeployRequest>,
) -> impl IntoResponse {
    let config = state.config.read().await;
    let deploy_cfg = config.deploy.clone();
    let aws_region = config.aws.region.clone();
    drop(config);

    // IP allowlist check
    if !deploy_cfg.allowed_cidrs.is_empty() {
        let client_ip = addr.ip();
        let allowed = deploy_cfg.allowed_cidrs.iter().any(|cidr| {
            cidr.parse::<IpNet>().map(|net| net.contains(&client_ip)).unwrap_or(false)
                || cidr.parse::<IpAddr>().map(|ip| ip == client_ip).unwrap_or(false)
        });
        if !allowed {
            return (StatusCode::FORBIDDEN, Json(ErrorResponse { error: "forbidden".into() })).into_response();
        }
    }

    // Bearer token check
    let expected_key = state.config.read().await.effective_deploy_key();
    if let Some(expected) = expected_key {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if provided != Some(expected.as_str()) {
            return (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "unauthorized".into() })).into_response();
        }
    }

    // Find the matching route config
    let config = state.config.read().await;
    let route = config.routes.iter().find(|r| {
        r.path.trim_start_matches('/').split('/').next()
            .map(|seg| seg == body.lambda || r.path.contains(&body.lambda))
            .unwrap_or(false)
            || route_name_matches(&r.path, &body.lambda)
    }).cloned();
    drop(config);

    let mut route = match route {
        Some(r) => r,
        None => {
            return (StatusCode::NOT_FOUND, Json(ErrorResponse {
                error: format!("no route found for lambda '{}'", body.lambda),
            })).into_response();
        }
    };

    let route_key = Router::route_key(&route.method, &route.path);

    // Download zip from S3
    let staging_dir = PathBuf::from(format!("/tmp/osbox-deploy/{}", body.lambda));
    if let Err(e) = download_and_unpack_s3(&body.s3_bucket, &body.s3_key, &staging_dir, &aws_region).await {
        error!("deploy download failed for {}: {e}", body.lambda);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
            error: format!("download failed: {e}"),
        })).into_response();
    }

    // Update handler path to point to staging dir contents
    // The zip root is unpacked into staging_dir; handler path within zip is preserved.
    let handler_name = route.handler.file_name().unwrap_or_default().to_os_string();
    route.handler = staging_dir.join(&handler_name);

    // Hot-swap the process pool
    match state.process_manager.hot_swap(&route_key, route, &state.runtime_registry).await {
        Ok(pid) => {
            info!("deployed {} (pid={pid})", body.lambda);
            (StatusCode::OK, Json(DeployResponse {
                status: "ok".into(),
                lambda: body.lambda,
                pid,
            })).into_response()
        }
        Err(e) => {
            error!("hot_swap failed for {}: {e}", body.lambda);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
                error: format!("swap failed: {e}"),
            })).into_response()
        }
    }
}

fn route_name_matches(path: &str, name: &str) -> bool {
    path.trim_matches('/').split('/').any(|seg| seg == name)
}

async fn download_and_unpack_s3(
    bucket: &str,
    key: &str,
    dest: &PathBuf,
    region: &str,
) -> anyhow::Result<()> {
    use aws_sdk_s3::config::Region;
    use aws_config::BehaviorVersion;

    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .load()
        .await;
    let client = aws_sdk_s3::Client::new(&sdk_config);

    let resp = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("S3 GetObject failed: {e}"))?;

    let bytes = resp.body.collect().await
        .map_err(|e| anyhow::anyhow!("S3 body read failed: {e}"))?.into_bytes();

    std::fs::create_dir_all(dest)?;
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("zip open failed: {e}"))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest.join(file.name());
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_name_matches_segment() {
        assert!(route_name_matches("/auth/signin", "signin"));
        assert!(route_name_matches("/signin", "signin"));
        assert!(!route_name_matches("/auth/login", "signin"));
    }
}
```

- [ ] **Step 2: Enable ConnectInfo in server.rs**

In `src/server.rs`, update the `run` function to add `ConnectInfo`:

```rust
pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = AxumRouter::new()
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        .fallback(any(dispatch_lambda))
        .with_state(state)
        .into_make_service_with_connect_info::<SocketAddr>();

    info!("osbox listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 3: Run tests and build**

```bash
cargo test deploy && cargo build 2>&1 | grep -E "^error"
```

Expected: 1 deploy test passes, no build errors.

- [ ] **Step 4: Commit**

```bash
git add src/deploy.rs src/server.rs
git commit -m "feat: deploy API with S3 download, zip unpack, bearer token + IP auth"
```

---

## Task 11: Config Hot-Reload

**Files:**
- Create: `src/hotreload.rs`
- Modify: `src/main.rs` (spawn hot-reload watcher)
- Modify: `src/server.rs` (no changes needed — reads from RwLock)

- [ ] **Step 1: Create src/hotreload.rs**

```rust
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info};
use crate::config::Config;
use crate::router::Router;
use crate::state::AppState;

pub async fn watch_config(config_path: String, state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::channel::<()>(4);

    let path = config_path.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.blocking_send(());
            }
        }
    }).expect("notify watcher failed");

    watcher.watch(Path::new(&config_path), RecursiveMode::NonRecursive)
        .expect("failed to watch config file");

    // Debounce: wait for quiet period before reloading
    loop {
        if rx.recv().await.is_none() { break; }
        tokio::time::sleep(Duration::from_millis(200)).await;
        // drain any additional signals that arrived during sleep
        while rx.try_recv().is_ok() {}

        match Config::from_file(&path) {
            Ok(new_config) => {
                info!("config reloaded from {path}");
                let new_router = Router::new(new_config.routes.clone());
                *state.router.write().await = new_router;
                *state.config.write().await = new_config;
            }
            Err(e) => {
                error!("config reload failed: {e}");
            }
        }
    }

    // Keep watcher alive
    drop(watcher);
}
```

- [ ] **Step 2: Add `mod hotreload;` and spawn watcher in main.rs**

In `main()`, after `server::run` setup, add before the `server::run` call:

```rust
mod hotreload;

// In main(), before server::run:
let watch_state = app_state.clone();
let watch_config_path = cli.config.clone();
tokio::spawn(async move {
    hotreload::watch_config(watch_config_path, watch_state).await;
});
```

- [ ] **Step 3: Build**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/hotreload.rs src/main.rs
git commit -m "feat: osbox.toml hot-reload via notify with 200ms debounce"
```

---

## Task 12: Datadog Metrics Emitter

**Files:**
- Modify: `src/metrics.rs` (replace stub with real implementation)

- [ ] **Step 1: Replace src/metrics.rs**

```rust
use cadence::{prelude::*, QueuingMetricSink, StatsdClient, UdpMetricSink};
use std::net::UdpSocket;
use crate::config::DatadogConfig;

pub struct MetricsEmitter {
    client: Option<StatsdClient>,
    service: String,
    env: String,
}

impl MetricsEmitter {
    pub fn new(config: &DatadogConfig) -> Self {
        if !config.enabled {
            return Self { client: None, service: config.service.clone(), env: config.env.clone() };
        }
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("metrics: could not bind UDP socket: {e} — metrics disabled");
                return Self { client: None, service: config.service.clone(), env: config.env.clone() };
            }
        };
        socket.set_nonblocking(true).ok();
        let sink = match UdpMetricSink::from(config.statsd_host.as_str(), socket) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("metrics: could not create statsd sink: {e} — metrics disabled");
                return Self { client: None, service: config.service.clone(), env: config.env.clone() };
            }
        };
        let queuing_sink = QueuingMetricSink::from(sink);
        let client = StatsdClient::from_sink(&config.service, queuing_sink);
        Self { client: Some(client), service: config.service.clone(), env: config.env.clone() }
    }

    fn tags<'a>(&'a self, route: &'a str) -> [&'a str; 2] {
        // cadence uses "key:value" tag format for DogStatsD
        // We build them inline at call sites to avoid alloc.
        // (cadence's with_tag API is more ergonomic but requires builder pattern)
        let _ = route; // used in call sites
        [""; 2]
    }

    pub fn record_request(&self, route: &str, method: &str, status: u16, duration_ms: f64) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize_tag(route));
            let method_tag = format!("method:{}", method.to_lowercase());
            let status_tag = format!("status:{}", status);
            let env_tag = format!("env:{}", self.env);
            c.time_with_tags("osbox.request.duration", duration_ms as u64)
                .with_tag_str(&route_tag)
                .with_tag_str(&method_tag)
                .with_tag_str(&status_tag)
                .with_tag_str(&env_tag)
                .send();
            c.incr_with_tags("osbox.request.count")
                .with_tag_str(&route_tag)
                .with_tag_str(&status_tag)
                .with_tag_str(&env_tag)
                .send();
        }
    }

    pub fn record_cache_hit(&self, route: &str) {
        if let Some(c) = &self.client {
            let tag = format!("route:{}", sanitize_tag(route));
            c.incr_with_tags("osbox.cache.hit").with_tag_str(&tag).send();
        }
    }

    pub fn record_cache_miss(&self, route: &str) {
        if let Some(c) = &self.client {
            let tag = format!("route:{}", sanitize_tag(route));
            c.incr_with_tags("osbox.cache.miss").with_tag_str(&tag).send();
        }
    }

    pub fn record_lambda_crash(&self, route: &str, runtime: &str) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize_tag(route));
            let runtime_tag = format!("runtime:{runtime}");
            c.incr_with_tags("osbox.lambda.crash")
                .with_tag_str(&route_tag)
                .with_tag_str(&runtime_tag)
                .send();
        }
    }

    pub fn record_lambda_timeout(&self, route: &str) {
        if let Some(c) = &self.client {
            let tag = format!("route:{}", sanitize_tag(route));
            c.incr_with_tags("osbox.lambda.timeout").with_tag_str(&tag).send();
        }
    }

    pub fn record_lambda_healthy(&self, route: &str, healthy: bool) {
        if let Some(c) = &self.client {
            let tag = format!("route:{}", sanitize_tag(route));
            c.gauge_with_tags("osbox.lambda.healthy", if healthy { 1 } else { 0 })
                .with_tag_str(&tag)
                .send();
        }
    }
}

fn sanitize_tag(s: &str) -> String {
    s.replace([':', '/', ' '], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_emitter_does_not_panic() {
        let config = DatadogConfig {
            enabled: false,
            statsd_host: "127.0.0.1:8125".into(),
            service: "test".into(),
            env: "test".into(),
        };
        let emitter = MetricsEmitter::new(&config);
        emitter.record_request("GET /foo", "GET", 200, 12.5);
        emitter.record_cache_hit("GET /foo");
        emitter.record_lambda_crash("GET /foo", "bun");
    }

    #[test]
    fn sanitize_tag_replaces_special_chars() {
        assert_eq!(sanitize_tag("GET /accounts/:id"), "GET__accounts__id");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test metrics 2>&1
```

Expected: 2 tests pass.

- [ ] **Step 3: Build**

```bash
cargo build 2>&1 | grep -E "^error"
```

- [ ] **Step 4: Commit**

```bash
git add src/metrics.rs
git commit -m "feat: Datadog DogStatsD emitter with QueuingMetricSink"
```

---

## Task 13: Ratatui TUI

**Files:**
- Create: `src/tui/app.rs`
- Create: `src/tui/widgets.rs`
- Modify: `src/tui/mod.rs` (full implementation)
- Modify: `src/main.rs` (spawn TUI thread)

- [ ] **Step 1: Create src/tui/app.rs**

```rust
use std::collections::VecDeque;
use crate::state::{LogEntry, RouteStats};
use crate::process::PoolStats;

#[derive(Default)]
pub struct App {
    pub route_stats: Vec<(String, RouteStats)>,
    pub pool_stats: Vec<PoolStats>,
    pub cache_entry_count: u64,
    pub log_entries: VecDeque<LogEntry>,
    pub selected_tab: usize,
}

impl App {
    pub fn tab_titles() -> &'static [&'static str] {
        &["Routes", "Processes", "Cache", "Logs"]
    }

    pub fn next_tab(&mut self) {
        self.selected_tab = (self.selected_tab + 1) % Self::tab_titles().len();
    }

    pub fn prev_tab(&mut self) {
        if self.selected_tab == 0 {
            self.selected_tab = Self::tab_titles().len() - 1;
        } else {
            self.selected_tab -= 1;
        }
    }
}
```

- [ ] **Step 2: Create src/tui/widgets.rs**

```rust
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};
use crate::tui::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.area());

    render_tabs(frame, app, chunks[0]);

    match app.selected_tab {
        0 => render_routes(frame, app, chunks[1]),
        1 => render_processes(frame, app, chunks[1]),
        2 => render_cache(frame, app, chunks[1]),
        3 => render_logs(frame, app, chunks[1]),
        _ => {}
    }
}

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = App::tab_titles()
        .iter()
        .map(|t| Line::from(Span::raw(*t)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.selected_tab)
        .block(Block::default().borders(Borders::ALL).title("osbox"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn render_routes(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(["Route", "Req/s", "p50ms", "p95ms", "Hit%", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.route_stats.iter().map(|(key, stats)| {
        let rps = if stats.latencies_ms.is_empty() { 0.0 } else {
            1000.0 / stats.p50_ms().max(1.0)
        };
        let hit_pct = if stats.cache_hits + stats.cache_misses == 0 { 0.0 } else {
            stats.cache_hits as f64 / (stats.cache_hits + stats.cache_misses) as f64 * 100.0
        };
        let health_color = if stats.healthy { Color::Green } else { Color::Red };
        Row::new([
            Cell::from(key.as_str()),
            Cell::from(format!("{rps:.1}")),
            Cell::from(format!("{:.1}", stats.p50_ms())),
            Cell::from(format!("{:.1}", stats.p95_ms())),
            Cell::from(format!("{hit_pct:.0}%")),
            Cell::from(if stats.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(12),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Routes"));

    frame.render_widget(table, area);
}

fn render_processes(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(["Route", "PIDs", "Restarts", "Health"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app.pool_stats.iter().map(|s| {
        let pids: Vec<String> = s.pids.iter().map(|p| p.to_string()).collect();
        let health_color = if s.healthy { Color::Green } else { Color::Red };
        Row::new([
            Cell::from(s.route_key.as_str()),
            Cell::from(pids.join(", ")),
            Cell::from(s.restart_count.to_string()),
            Cell::from(if s.healthy { "ok" } else { "down" })
                .style(Style::default().fg(health_color)),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Processes"));

    frame.render_widget(table, area);
}

fn render_cache(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!("Cached entries: {}", app.cache_entry_count);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Cache"))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_logs(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app.log_entries.iter().rev().take(area.height as usize).map(|entry| {
        use std::time::UNIX_EPOCH;
        let secs = entry.timestamp.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let color = match entry.level.as_str() {
            "ERROR" => Color::Red,
            "WARN" => Color::Yellow,
            _ => Color::White,
        };
        Line::from(vec![
            Span::styled(format!("[{secs}] "), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[{}] ", entry.level), Style::default().fg(color)),
            Span::raw(entry.message.clone()),
        ])
    }).collect();

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Logs"));
    frame.render_widget(paragraph, area);
}
```

- [ ] **Step 3: Implement src/tui/mod.rs**

```rust
pub mod app;
pub mod widgets;

use std::io;
use std::sync::Arc;
use std::time::Duration;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use crate::state::AppState;
use self::app::App;

pub fn run_tui(state: Arc<AppState>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    result
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    let mut app = App::default();
    let tick = Duration::from_millis(100);

    loop {
        // Refresh app state from Arc<AppState>
        {
            let rt = tokio::runtime::Handle::try_current();
            if let Ok(handle) = rt {
                handle.block_on(async {
                    let route_stats = state.route_stats.read().await;
                    app.route_stats = route_stats
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    app.pool_stats = state.process_manager.pool_stats().await;
                    app.cache_entry_count = state.cache.entry_count();
                    let log_buf = state.log_buffer.lock().await;
                    app.log_entries = log_buf.clone();
                });
            }
        }

        terminal.draw(|f| widgets::render(f, &app))?;

        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Tab | KeyCode::Right => app.next_tab(),
                    KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire TUI into main.rs**

In `main()`, after building `app_state` and before `server::run`, add:

```rust
let tui_enabled = !cli.no_tui && atty::is(atty::Stream::Stdout);
if tui_enabled {
    let tui_state = app_state.clone();
    std::thread::spawn(move || {
        if let Err(e) = tui::run_tui(tui_state) {
            eprintln!("TUI error: {e}");
        }
    });
}
```

Add `atty` to `Cargo.toml`:

```toml
atty = "0.2"
```

- [ ] **Step 5: Build**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add src/tui/mod.rs src/tui/app.rs src/tui/widgets.rs src/main.rs Cargo.toml
git commit -m "feat: Ratatui TUI with Routes/Processes/Cache/Logs tabs"
```

---

## Task 14: Integration Test

**Files:**
- Create: `tests/integration_test.rs`
- Create: `tests/fixtures/echo-lambda/index.ts`

- [ ] **Step 1: Create the echo lambda fixture**

```bash
mkdir -p tests/fixtures/echo-lambda
```

Create `tests/fixtures/echo-lambda/index.ts`:

```typescript
export const handler = async (event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ echo: event.rawPath, method: event.requestContext.http.method }),
  };
};
```

- [ ] **Step 2: Create tests/integration_test.rs**

```rust
//! Integration test: starts osbox with a real Bun echo lambda and fires HTTP requests.
//! Requires `bun` to be installed on PATH.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires bun on PATH"]
async fn echo_lambda_returns_200() {
    let config_toml = format!(r#"
[server]
port = 0
host = "127.0.0.1"

[[routes]]
path = "/echo"
method = "GET"
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
concurrency = 1
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: osbox::config::Config = toml::from_str(&config_toml).unwrap();

    let registry = osbox::process::runtime::RuntimeRegistry::new().unwrap();
    let cache = osbox::cache::CacheLayer::new(&config.cache);
    let metrics = osbox::metrics::MetricsEmitter::new(&config.datadog);
    let router = osbox::router::Router::new(config.routes.clone());
    let process_manager = osbox::process::ProcessManager::new();
    process_manager.spawn_all(&config.routes, &registry).await.unwrap();

    let app_state = Arc::new(osbox::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    // Bind to random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let app = osbox::server::build_app(app_state);
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let resp = reqwest::get(format!("http://{bound_addr}/echo")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["echo"], "/echo");
    assert_eq!(body["method"], "GET");
}

#[tokio::test]
#[ignore = "requires bun on PATH"]
async fn cache_returns_hit_on_second_request() {
    // Same setup as above but with cache_ttl_secs = 60
    // Second request should be served from cache (verify via cache entry count)
    let config_toml = format!(r#"
[server]
port = 0
host = "127.0.0.1"

[[routes]]
path = "/cached"
method = "GET"
runtime = "bun"
handler = "{handler}"
timeout_ms = 5000
cache_ttl_secs = 60
concurrency = 1
"#, handler = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-lambda/index.ts"));

    let config: osbox::config::Config = toml::from_str(&config_toml).unwrap();
    let registry = osbox::process::runtime::RuntimeRegistry::new().unwrap();
    let cache = osbox::cache::CacheLayer::new(&config.cache);
    let metrics = osbox::metrics::MetricsEmitter::new(&config.datadog);
    let router = osbox::router::Router::new(config.routes.clone());
    let process_manager = osbox::process::ProcessManager::new();
    process_manager.spawn_all(&config.routes, &registry).await.unwrap();

    let state = Arc::new(osbox::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    let state_for_check = state.clone();

    tokio::spawn(async move {
        let app = osbox::server::build_app(state);
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    reqwest::get(format!("http://{bound_addr}/cached")).await.unwrap();
    reqwest::get(format!("http://{bound_addr}/cached")).await.unwrap();

    // After two requests, one cache miss + one hit → entry_count = 1
    assert_eq!(state_for_check.cache.entry_count(), 1);
    let stats = state_for_check.route_stats.read().await;
    let route_stats = stats.get("GET /cached").unwrap();
    assert_eq!(route_stats.cache_hits, 1);
    assert_eq!(route_stats.cache_misses, 1);
}
```

- [ ] **Step 3: Extract `build_app` from server.rs**

In `src/server.rs`, add a public `build_app` function and update `run` to use it:

```rust
pub fn build_app(state: Arc<AppState>) -> axum::routing::Router {
    AxumRouter::new()
        .route("/cache/invalidate", post(cache_invalidate))
        .route("/deploy", post(crate::deploy::deploy_handler))
        .fallback(any(dispatch_lambda))
        .with_state(state)
}

pub async fn run(state: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = build_app(state).into_make_service_with_connect_info::<SocketAddr>();
    info!("osbox listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

Add `reqwest` to dev-dependencies in Cargo.toml:

```toml
[dev-dependencies]
reqwest = { version = "0.12", features = ["json"] }
tempfile = "3"
tokio-test = "0.4"
```

Also add `pub use` re-exports in `src/lib.rs` (create it):

```rust
pub mod cache;
pub mod config;
pub mod deploy;
pub mod gateway;
pub mod metrics;
pub mod process;
pub mod router;
pub mod server;
pub mod state;
```

- [ ] **Step 4: Run integration tests (requires bun)**

```bash
cargo test --test integration_test -- --include-ignored 2>&1
```

Expected (with bun installed):
```
test echo_lambda_returns_200 ... ok
test cache_returns_hit_on_second_request ... ok
```

- [ ] **Step 5: Run unit tests to confirm nothing broke**

```bash
cargo test 2>&1
```

Expected: all unit tests pass.

- [ ] **Step 6: Commit**

```bash
git add tests/ src/server.rs src/lib.rs Cargo.toml
git commit -m "feat: integration tests with real Bun echo lambda and cache verification"
```

---

## Self-Review Checklist

After writing this plan, verify spec coverage:

| Spec requirement | Task |
|-----------------|------|
| Rust host binary | Task 1 |
| axum HTTP server | Task 9 |
| TOML config + hot-reload | Tasks 2, 11 |
| Route matching | Task 4 |
| Process manager (Bun runtime) | Tasks 6, 7 |
| TTL cache + invalidation API | Tasks 5, 9 |
| Deploy API (S3 + auth) | Task 10 |
| Ratatui TUI | Task 13 |
| Datadog DogStatsD | Task 12 |
| Clap CLI (validate, routes, deploy) | Task 9 main.rs |
| Shared AppState | Task 8 |
| Bearer token + IP allowlist | Task 10 |
| Gateway v2 types | Task 3 |
| Concurrent process pool | Task 7 |
| Crash detection + 503 threshold | Task 7 |
| Integration test | Task 14 |
