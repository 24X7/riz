# Examples Folder and Dev Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three realistic Bun lambda examples, dev/prod TOML configs, and a `--dev` CLI flag that switches between colorized+TUI dev mode and JSON-stdout prod mode.

**Architecture:** Lambda files are plain TypeScript using the AWS HTTP Gateway v2 event shape. The two TOML configs point at the same lambdas with different cache/timeout/log settings. The `--dev` flag in `src/main.rs` controls log format (pretty vs JSON), default config file, log level default, and TUI behavior.

**Tech Stack:** Bun/TypeScript (lambdas), TOML (configs), Rust/Clap (CLI flag), tracing-subscriber with `json` feature (structured logging).

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `examples/lambdas/ping/index.ts` | Create | GET /ping — returns status + timestamp |
| `examples/lambdas/accounts/index.ts` | Create | GET /accounts/:id — path param + query string |
| `examples/lambdas/events/index.ts` | Create | POST /events — JSON body in/out |
| `examples/osbox.dev.toml` | Create | Dev config: no cache, 1000ms timeouts, 127.0.0.1 |
| `examples/osbox.prod.toml` | Create | Prod config: cache on accounts, 5000ms timeouts, 0.0.0.0 |
| `examples/README.md` | Create | How to run both modes + curl examples |
| `src/main.rs` | Modify | Add `--dev` flag, logging modes, TUI override, config default |
| `Cargo.toml` | Modify | Add `json` feature to `tracing-subscriber` |

---

## Task 1: ping lambda

**Files:**
- Create: `examples/lambdas/ping/index.ts`

- [ ] **Step 1: Create the examples directory structure**

```bash
mkdir -p examples/lambdas/ping
```

- [ ] **Step 2: Create `examples/lambdas/ping/index.ts`**

```typescript
export const handler = async (_event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ status: "ok", ts: Date.now() }),
  };
};
```

- [ ] **Step 3: Verify it runs standalone with bun**

```bash
echo '{"version":"2.0","routeKey":"GET /ping","rawPath":"/ping","rawQueryString":"","headers":{},"requestContext":{"http":{"method":"GET","path":"/ping","protocol":"HTTP/1.1","sourceIp":"127.0.0.1"},"requestId":"test-1","timeEpoch":1000},"isBase64Encoded":false}' \
  | bun run examples/lambdas/ping/index.ts 2>&1 || true
```

Note: this won't work directly (the file needs the bun adapter). The real verification is in Task 4 when you run osbox with the dev config. Just confirm the file syntax is valid:

```bash
bun --eval "import('./examples/lambdas/ping/index.ts').then(m => console.log(typeof m.handler))"
```

Expected: `function`

- [ ] **Step 4: Commit**

```bash
git add examples/lambdas/ping/index.ts
git commit -m "feat: add ping example lambda"
```

---

## Task 2: accounts lambda

**Files:**
- Create: `examples/lambdas/accounts/index.ts`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p examples/lambdas/accounts
```

- [ ] **Step 2: Create `examples/lambdas/accounts/index.ts`**

```typescript
export const handler = async (event: any, _ctx: any) => {
  const id = event.pathParameters?.id ?? "unknown";

  // Parse rawQueryString: "include=profile&verbose=true" → { include: "profile", verbose: "true" }
  const params: Record<string, string> = {};
  if (event.rawQueryString) {
    for (const pair of event.rawQueryString.split("&")) {
      const [k, v] = pair.split("=");
      if (k) params[decodeURIComponent(k)] = decodeURIComponent(v ?? "");
    }
  }

  const account = {
    id,
    name: `Account ${id}`,
    plan: "pro",
    include: params.include ?? null,
    ts: Date.now(),
  };

  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(account),
  };
};
```

- [ ] **Step 3: Verify syntax**

```bash
bun --eval "import('./examples/lambdas/accounts/index.ts').then(m => console.log(typeof m.handler))"
```

Expected: `function`

- [ ] **Step 4: Commit**

```bash
git add examples/lambdas/accounts/index.ts
git commit -m "feat: add accounts example lambda"
```

---

## Task 3: events lambda

**Files:**
- Create: `examples/lambdas/events/index.ts`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p examples/lambdas/events
```

- [ ] **Step 2: Create `examples/lambdas/events/index.ts`**

```typescript
export const handler = async (event: any, _ctx: any) => {
  if (!event.body) {
    return {
      statusCode: 400,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ error: "body required" }),
    };
  }

  let payload: unknown;
  try {
    payload = JSON.parse(event.body);
  } catch {
    return {
      statusCode: 400,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ error: "body must be valid JSON" }),
    };
  }

  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ received: payload, confirmedAt: Date.now() }),
  };
};
```

- [ ] **Step 3: Verify syntax**

```bash
bun --eval "import('./examples/lambdas/events/index.ts').then(m => console.log(typeof m.handler))"
```

Expected: `function`

- [ ] **Step 4: Commit**

```bash
git add examples/lambdas/events/index.ts
git commit -m "feat: add events example lambda"
```

---

## Task 4: TOML configs

**Files:**
- Create: `examples/osbox.dev.toml`
- Create: `examples/osbox.prod.toml`

- [ ] **Step 1: Create `examples/osbox.dev.toml`**

```toml
[server]
port = 3000
host = "127.0.0.1"

[cache]
default_ttl_secs = 0
max_size_mb = 128

[datadog]
enabled = false

[[routes]]
path = "/ping"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/ping/index.ts"
timeout_ms = 1000
concurrency = 1

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/accounts/index.ts"
timeout_ms = 1000
concurrency = 1

[[routes]]
path = "/events"
method = "POST"
runtime = "bun"
handler = "./examples/lambdas/events/index.ts"
timeout_ms = 1000
concurrency = 1
```

- [ ] **Step 2: Create `examples/osbox.prod.toml`**

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
allowed_cidrs = []

[aws]
region = "us-east-1"

[[routes]]
path = "/ping"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/ping/index.ts"
timeout_ms = 5000
concurrency = 2

[[routes]]
path = "/accounts/:id"
method = "GET"
runtime = "bun"
handler = "./examples/lambdas/accounts/index.ts"
cache_ttl_secs = 30
timeout_ms = 5000
concurrency = 2

[[routes]]
path = "/events"
method = "POST"
runtime = "bun"
handler = "./examples/lambdas/events/index.ts"
timeout_ms = 5000
concurrency = 2
```

- [ ] **Step 3: Validate both configs compile correctly**

Build the binary first if not already built:
```bash
cargo build 2>&1 | grep "^error" | head -5
```

Then validate:
```bash
cargo run -- validate --config examples/osbox.dev.toml 2>&1
cargo run -- validate --config examples/osbox.prod.toml 2>&1
```

Expected for each:
```
Config OK: 3 routes
```

- [ ] **Step 4: Commit**

```bash
git add examples/osbox.dev.toml examples/osbox.prod.toml
git commit -m "feat: add dev and prod example TOML configs"
```

---

## Task 5: `--dev` flag, logging modes, TUI override

**Files:**
- Modify: `Cargo.toml` (add `json` feature to `tracing-subscriber`)
- Modify: `src/main.rs` (full rewrite of CLI struct and main setup logic)

- [ ] **Step 1: Add `json` feature to `tracing-subscriber` in `Cargo.toml`**

Change:
```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
```

To:
```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

- [ ] **Step 2: Write failing tests for CLI parsing and config resolution**

Add at the bottom of `src/main.rs` (before the final closing brace of the file, after the `main` function):

```rust
fn effective_config_path(dev: bool, explicit: Option<&str>) -> String {
    explicit.map(|s| s.to_string()).unwrap_or_else(|| {
        if dev { "osbox.dev.toml".into() } else { "osbox.toml".into() }
    })
}

fn effective_log_level<'a>(dev: bool, explicit: Option<&'a str>) -> &'a str {
    explicit.unwrap_or(if dev { "debug" } else { "info" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_flag_parsed() {
        let cli = Cli::try_parse_from(["osbox", "--dev"]).unwrap();
        assert!(cli.dev);
        assert!(cli.config.is_none());
        assert!(cli.log_level.is_none());
    }

    #[test]
    fn no_dev_flag_by_default() {
        let cli = Cli::try_parse_from(["osbox"]).unwrap();
        assert!(!cli.dev);
    }

    #[test]
    fn explicit_config_overrides_dev_default() {
        let cli = Cli::try_parse_from(["osbox", "--dev", "--config", "custom.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("custom.toml"));
        assert_eq!(effective_config_path(cli.dev, cli.config.as_deref()), "custom.toml");
    }

    #[test]
    fn config_defaults_by_mode() {
        assert_eq!(effective_config_path(true, None), "osbox.dev.toml");
        assert_eq!(effective_config_path(false, None), "osbox.toml");
    }

    #[test]
    fn log_level_defaults_by_mode() {
        assert_eq!(effective_log_level(true, None), "debug");
        assert_eq!(effective_log_level(false, None), "info");
        assert_eq!(effective_log_level(true, Some("warn")), "warn");
    }
}
```

- [ ] **Step 3: Run tests to confirm they fail (functions not yet defined in the right place)**

```bash
cargo test main 2>&1 | tail -10
```

Expected: compile errors because `Cli` doesn't have `dev` field yet and the helper functions aren't defined yet in the right context.

- [ ] **Step 4: Update `src/main.rs` with the full new implementation**

Replace the entire contents of `src/main.rs`:

```rust
mod cache;
mod config;
mod deploy;
mod gateway;
mod hotreload;
mod metrics;
mod process;
mod router;
mod server;
mod state;
mod tui;

use std::net::SocketAddr;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "osbox", about = "Self-hosted AWS Lambda host")]
struct Cli {
    /// Config file. Defaults to osbox.dev.toml in --dev mode, osbox.toml otherwise.
    #[arg(short, long)]
    config: Option<String>,

    #[arg(short, long)]
    port: Option<u16>,

    #[arg(long)]
    no_tui: bool,

    /// Log level. Defaults to debug in --dev mode, info otherwise.
    #[arg(long)]
    log_level: Option<String>,

    /// Developer mode: colorized logs, debug level, TUI always on, defaults to osbox.dev.toml.
    #[arg(long)]
    dev: bool,

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

fn effective_config_path(dev: bool, explicit: Option<&str>) -> String {
    explicit.map(|s| s.to_string()).unwrap_or_else(|| {
        if dev { "osbox.dev.toml".into() } else { "osbox.toml".into() }
    })
}

fn effective_log_level<'a>(dev: bool, explicit: Option<&'a str>) -> &'a str {
    explicit.unwrap_or(if dev { "debug" } else { "info" })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config_path = effective_config_path(cli.dev, cli.config.as_deref());
    let log_level = effective_log_level(cli.dev, cli.log_level.as_deref());
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    if cli.dev {
        tracing_subscriber::fmt()
            .pretty()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    }

    let config = config::Config::from_file(&config_path)?;

    match &cli.command {
        Some(Commands::Validate) => {
            println!("Config OK: {} routes", config.routes.len());
            return Ok(());
        }
        Some(Commands::Routes) => {
            for route in &config.routes {
                println!("{} {} -> {:?} ({})",
                    route.method, route.path,
                    route.handler, route.runtime.as_str());
            }
            return Ok(());
        }
        _ => {}
    }

    let port = cli.port.unwrap_or(config.server.port);
    let host: std::net::IpAddr = config.server.host.parse()?;
    let addr = SocketAddr::new(host, port);

    let registry = Arc::new(process::runtime::RuntimeRegistry::new()?);
    let cache = cache::CacheLayer::new(&config.cache);
    let metrics = metrics::MetricsEmitter::new(&config.datadog);
    let router = router::Router::new(config.routes.clone());
    let process_manager = process::ProcessManager::new();

    if config.effective_deploy_key().is_none() {
        tracing::warn!("SECURITY: no deploy key configured — POST /deploy is unauthenticated");
    }

    process_manager.spawn_all(&config.routes, &registry).await?;

    let app_state = Arc::new(state::AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        metrics,
        runtime_registry: registry,
        route_stats: tokio::sync::RwLock::new(Default::default()),
        log_buffer: tokio::sync::Mutex::new(Default::default()),
    });

    // Dev mode forces TUI on regardless of --no-tui and atty check.
    // Prod mode: TUI only if stdout is a TTY and --no-tui not set.
    let tui_enabled = if cli.dev {
        true
    } else {
        !cli.no_tui && std::io::IsTerminal::is_terminal(&std::io::stdout())
    };

    if tui_enabled {
        let tui_state = app_state.clone();
        let tui_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            if let Err(e) = tui::run_tui(tui_state, tui_handle) {
                eprintln!("TUI error: {e}");
            }
        });
    }

    let watch_state = app_state.clone();
    let watch_config_path = config_path.clone();
    tokio::spawn(async move {
        hotreload::watch_config(watch_config_path, watch_state).await;
    });

    if cli.dev {
        info!("osbox starting in [dev] mode on {addr}");
    } else {
        info!(mode = "production", addr = %addr, "osbox starting");
    }

    server::run(app_state, addr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_flag_parsed() {
        let cli = Cli::try_parse_from(["osbox", "--dev"]).unwrap();
        assert!(cli.dev);
        assert!(cli.config.is_none());
        assert!(cli.log_level.is_none());
    }

    #[test]
    fn no_dev_flag_by_default() {
        let cli = Cli::try_parse_from(["osbox"]).unwrap();
        assert!(!cli.dev);
    }

    #[test]
    fn explicit_config_overrides_dev_default() {
        let cli = Cli::try_parse_from(["osbox", "--dev", "--config", "custom.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("custom.toml"));
        assert_eq!(effective_config_path(cli.dev, cli.config.as_deref()), "custom.toml");
    }

    #[test]
    fn config_defaults_by_mode() {
        assert_eq!(effective_config_path(true, None), "osbox.dev.toml");
        assert_eq!(effective_config_path(false, None), "osbox.toml");
    }

    #[test]
    fn log_level_defaults_by_mode() {
        assert_eq!(effective_log_level(true, None), "debug");
        assert_eq!(effective_log_level(false, None), "info");
        assert_eq!(effective_log_level(true, Some("warn")), "warn");
    }
}
```

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | tail -5
```

Expected:
```
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.Xs
```

(29 existing + 5 new = 34 total)

- [ ] **Step 6: Verify prod mode produces JSON logs**

```bash
cargo run -- validate --config examples/osbox.dev.toml 2>&1
```

Expected: pretty-printed tracing output (colored if terminal supports it), ends with `Config OK: 3 routes`.

```bash
cargo run -- --config examples/osbox.dev.toml validate 2>&1
```

Expected: same — `--config` before subcommand also works.

```bash
cargo run -- validate --config examples/osbox.prod.toml 2>&1
```

Expected: JSON log line like `{"timestamp":"...","level":"INFO","fields":{"message":"..."},...}` followed by `Config OK: 3 routes` on stdout.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs Cargo.toml
git commit -m "feat: --dev flag with colorized TUI mode vs JSON stdout prod mode"
```

---

## Task 6: examples/README.md

**Files:**
- Create: `examples/README.md`

- [ ] **Step 1: Create `examples/README.md`**

```markdown
# osbox Examples

Three example Bun lambda handlers demonstrating the core input/output patterns.

## Prerequisites

- [bun](https://bun.sh) installed and on `PATH`
- osbox binary built: `cargo build --release` (or `cargo build` for dev)

## Dev Mode

Colorized logs, debug level, TUI dashboard always on, hot-reload of config:

```bash
cargo run -- --dev
```

Loads `examples/osbox.dev.toml` by default. All three routes are available:

```bash
# Health check — no input
curl http://localhost:3000/ping

# Path param + query string
curl "http://localhost:3000/accounts/42?include=profile"

# JSON body
curl -X POST http://localhost:3000/events \
  -H "content-type: application/json" \
  -d '{"type":"signup","userId":"abc123"}'
```

### Hot-Reload

While `osbox --dev` is running, edit `examples/osbox.dev.toml` — change a timeout,
add a route, or remove one. The TUI Routes tab updates within ~200ms without
restarting the host.

## Prod Mode

JSON-structured stdout logs, no TUI, caching enabled on `GET /accounts/:id`:

```bash
cargo run -- --config examples/osbox.prod.toml
```

Same curl commands work. The second request to `/accounts/:id` is served from
cache — watch the Cache tab hit count increment if you run with `--config examples/osbox.prod.toml`
and a terminal (atty detected).

## Cache Invalidation

```bash
curl -X POST http://localhost:3000/cache/invalidate \
  -H "content-type: application/json" \
  -d '{"prefix":"GET:/accounts/"}'
```

Returns `{"evicted": N}` with the number of entries cleared.

## Lambda Structure

Each lambda is a standard AWS HTTP Gateway v2 handler:

```typescript
export const handler = async (event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ... }),
  };
};
```

osbox routes requests to these handlers via stdin/stdout — no SDK changes needed.
The `handler` field in `osbox.toml` points directly to the `.ts` file; naming is
entirely up to you.
```

- [ ] **Step 2: Commit**

```bash
git add examples/README.md
git commit -m "docs: examples README with curl commands and hot-reload instructions"
```

---

## Self-Review Checklist

| Spec requirement | Task |
|-----------------|------|
| `examples/lambdas/ping/index.ts` — no input, status+ts | Task 1 |
| `examples/lambdas/accounts/index.ts` — path param + query string | Task 2 |
| `examples/lambdas/events/index.ts` — JSON body in/out, 400 on bad JSON | Task 3 |
| `examples/osbox.dev.toml` — no cache, 1000ms, 127.0.0.1 | Task 4 |
| `examples/osbox.prod.toml` — cache on accounts, 5000ms, 0.0.0.0 | Task 4 |
| `--dev` flag in Clap CLI | Task 5 |
| Dev: colorized pretty logs, debug level default | Task 5 |
| Prod: JSON stdout logs, info level default | Task 5 |
| Dev: TUI always on, ignores `--no-tui` and atty | Task 5 |
| Dev: defaults to `osbox.dev.toml` | Task 5 |
| Prod: defaults to `osbox.toml` | Task 5 |
| Startup banner with mode | Task 5 |
| `examples/README.md` with curl examples and hot-reload note | Task 6 |
