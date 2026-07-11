mod auth;
mod broker;
mod cache;
mod config;
mod cors;
mod deploy;
mod gateway;
mod hotreload;
mod llm;
mod observability;
mod process;
mod router;
mod runtime;
mod scaffold;
mod server;
mod state;
mod static_files;
mod system;
mod template_fetch;
mod tui;
mod tui_log_layer;
mod ws;

#[cfg(test)]
mod test_helpers;

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "riz",
    version,
    about = "Self-hosted AWS Lambda runtime — HTTP API v2 + WebSocket, MCP-native",
    after_help = "\
Getting started:
  riz init typescript-http my-app   scaffold a project (then: cd my-app && riz run)
  riz init --list                   list the available templates
  riz run                           start ./riz.toml in the current directory
  riz --dev run                     start with the live TUI dashboard
  riz doctor                        pre-flight check your riz.toml + runtimes

Every function is also an MCP tool at /_riz/mcp.  Docs: https://riz.dev"
)]
struct Cli {
    /// Config file. Defaults to ./riz.toml in the current directory.
    /// No implicit fallback — if you want a different file, pass it.
    #[arg(short, long)]
    config: Option<String>,

    #[arg(short, long)]
    port: Option<u16>,

    /// Log level. Defaults to debug in --dev mode, info otherwise.
    #[arg(long)]
    log_level: Option<String>,

    /// Developer mode: TUI on, debug log level. Has no effect on which
    /// config is loaded — pass --config explicitly when working inside
    /// this repo (e.g. --config examples/riz.dev.toml).
    #[arg(long)]
    dev: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the runtime. Default when no subcommand is given.
    Run,
    /// Validate riz.toml and exit.
    Validate,
    /// List configured functions and their routes.
    Routes,
    /// Hot-swap a deployed function from S3.
    Deploy {
        lambda: String,
        s3_bucket: String,
        s3_key: String,
    },
    /// MCP utilities (inspect a running Riz instance, etc.).
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// A2A utilities — talk to any agent2agent server (a riz [agent] or any
    /// other A2A implementation).
    A2a {
        #[command(subcommand)]
        cmd: A2aCmd,
    },
    /// Pre-flight diagnostic: validates riz.toml, checks runtime binaries
    /// (bun / python3), confirms each function's handler file is present,
    /// probes the configured port, and (if riz is already running) pings
    /// the MCP endpoint. Designed to be the first command a user runs
    /// when "it won't start" so the failure surface is small + obvious.
    Doctor,
    /// Scaffold a new riz project by FETCHING a template from git.
    ///
    /// Templates are never embedded in the binary — they always load from a
    /// git location. `<spec>` may be:
    ///   - an official template name (`riz init --list`), e.g. `typescript-http`,
    ///     `typescript-todo` — fetched from a subdir of the riz repo;
    ///   - `owner/repo`, `owner/repo/subdir`, optionally `#ref` — any GitHub
    ///     repo or subdirectory, so you can use your own;
    ///   - a git URL (`https://…`, `git@…`, `file://…`) or a local path.
    ///
    /// Files are written into <dir> (defaults to the current directory). E.g.:
    /// `riz init typescript-todo my-app && cd my-app && riz run`.
    ///
    /// `riz init --list` prints the official templates. Set `RIZ_TEMPLATE_REPO`
    /// to fetch the official names from a fork.
    Init {
        /// Template spec (name / owner/repo[/subdir] / git URL / local path).
        /// Required unless --list is given.
        spec: Option<String>,
        /// Target directory (defaults to current dir).
        dir: Option<String>,
        /// git ref (branch / tag) to fetch. Overrides any `#ref` in the spec.
        #[arg(long)]
        r#ref: Option<String>,
        /// Print the official templates and exit. No scaffold is written.
        #[arg(long)]
        list: bool,
        /// Copy into a non-empty target directory (overwrites colliding files).
        #[arg(long)]
        force: bool,
        /// After scaffold, run `git init` + initial commit in the target
        /// directory. Skipped silently if git is not on PATH or the dir
        /// is already inside a git repo.
        #[arg(long)]
        git: bool,
    },
    /// Scaffold the agent-discovery static surface from your riz.toml.
    ///
    /// Generates a site root (default `public/`) containing `llms.txt` and
    /// `.well-known/riz.json` DERIVED from your functions — every
    /// `[function.*]` becomes a tool entry matching what `/_riz/mcp`
    /// advertises. Pair it with a `[static]` block (or pass `--wire` to add
    /// one) so a live instance serves these files itself: an agent pointed at
    /// your host then discovers its tools and the MCP endpoint with no
    /// separate marketing site.
    Scaffold {
        #[command(subcommand)]
        what: ScaffoldCmd,
    },
}

#[derive(Subcommand)]
enum ScaffoldCmd {
    /// Generate `<dir>/llms.txt` + `<dir>/.well-known/riz.json` from the
    /// current config. Refuses to overwrite existing files without --force.
    Static {
        /// Site root to write into (defaults to `public`).
        dir: Option<String>,
        /// Mount value written into the `[static]` block with --wire.
        #[arg(long, default_value = "/")]
        mount: String,
        /// Append a `[static]` block to riz.toml (pointing at <dir>) unless
        /// one is already configured.
        #[arg(long)]
        wire: bool,
        /// Overwrite existing generated files.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum A2aCmd {
    /// SendMessage to an A2A server and print the resulting task — state,
    /// answer artifact, and task id. `<base>` is the server root (e.g.
    /// `http://localhost:3000`); the endpoint is discovered from its Agent
    /// Card, falling back to `<base>/_riz/a2a`.
    Send {
        /// A2A server base URL.
        base: String,
        /// The message text to delegate.
        message: String,
        /// Bearer token for auth-gated endpoints. Reads $RIZ_AUTH_BEARER_TOKEN
        /// when omitted.
        #[arg(long)]
        bearer: Option<String>,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Connect to a running Riz instance and print the MCP server's
    /// capabilities + registered tools. The lowest-friction way to
    /// verify your MCP setup before pointing Claude or Cursor at it.
    ///
    /// Defaults to `http://localhost:3000/_riz/mcp`. Pass --url to point
    /// at a remote instance; pass --bearer when the endpoint is auth-gated.
    Inspect {
        /// MCP endpoint URL.
        #[arg(long, default_value = "http://localhost:3000/_riz/mcp")]
        url: String,
        /// Bearer token for auth-gated endpoints. Reads $RIZ_AUTH_BEARER_TOKEN
        /// when omitted.
        #[arg(long)]
        bearer: Option<String>,
    },
}

fn print_template_list() {
    println!("Official templates (fetched from git, never embedded):\n");
    println!("  {:<24} {:<12} LANGUAGE", "TEMPLATE", "SCENARIO");
    for (name, _subdir, scenario, lang) in template_fetch::BUILTINS {
        println!("  {name:<24} {scenario:<12} {lang}");
    }
    println!("\nUsage:");
    println!("  riz init <template> [dir] [--ref <ref>] [--git]   # an official template above");
    println!("  riz init <owner>/<repo>[/<subdir>][#ref] [dir]    # any GitHub repo or subdir");
    println!("  riz init <git-url|local-path> [dir]               # any git URL or local path");
    println!("\nTemplates always load from a git location — set RIZ_TEMPLATE_REPO to use a fork.");
}

/// Scaffold a project by FETCHING a template from a git location (or local
/// path) — nothing is embedded in the binary. `spec` is a built-in name, an
/// `owner/repo[/subdir][#ref]` shorthand, a git URL, or a local path.
fn run_init(
    spec: &str,
    dir: Option<&str>,
    reference: Option<&str>,
    git: bool,
    force: bool,
) -> anyhow::Result<()> {
    let source = template_fetch::resolve(spec, reference)?;
    let target = match dir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir()?,
    };

    println!("Fetching template {spec:?} → {}", target.display());
    let written = template_fetch::fetch_into(&source, &target, force)?;
    println!(
        "  copied {written} file{}",
        if written == 1 { "" } else { "s" }
    );

    if git {
        try_git_init(&target);
    }

    print_next_steps(spec, &target);
    Ok(())
}

/// `riz scaffold static` — load the project config and DERIVE the
/// agent-discovery surface (`<dir>/llms.txt` + `<dir>/.well-known/riz.json`)
/// from its functions. With `--wire`, also add a `[static]` block so a live
/// instance serves the generated files itself.
fn run_scaffold_static(
    config_path: &str,
    dir: Option<&str>,
    mount: &str,
    wire: bool,
    force: bool,
) -> anyhow::Result<()> {
    let config = config::Config::from_file(config_path).map_err(|e| {
        anyhow::anyhow!(
            "could not load {config_path}: {e}\n\
             `riz scaffold static` derives the tool list from your functions, so it \
             needs a valid riz.toml. Run it from your project dir or pass --config."
        )
    })?;
    config
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid riz.toml: {e}"))?;

    let target = std::path::PathBuf::from(dir.unwrap_or("public"));
    let opts = scaffold::ScaffoldOptions {
        dir: target.clone(),
        mount: mount.to_string(),
        wire,
        force,
    };
    let result = scaffold::scaffold_static(&config, std::path::Path::new(config_path), &opts)?;

    let n = config.functions.len();
    println!(
        "✓ scaffolded the agent-discovery surface from {config_path} ({n} function{} → tools)",
        if n == 1 { "" } else { "s" }
    );
    for p in &result.written {
        println!("  created {}", p.display());
    }
    if wire {
        if result.wired {
            println!(
                "  wired [static] into {config_path} (dir = {:?}, mount = {:?})",
                target.display().to_string(),
                mount
            );
        } else {
            println!("  [static] already configured — left {config_path} unchanged");
        }
    }
    println!("\n  Next steps:");
    if !wire && config.static_site.is_none() {
        println!(
            "    add a [static] block pointing dir at {:?} (or re-run with --wire)",
            target.display().to_string()
        );
    }
    println!(
        "    riz run    # GET /llms.txt and /.well-known/riz.json are now served by the instance"
    );
    println!();
    Ok(())
}

/// `git init` + initial commit in `target`. Best-effort: silently skips if
/// git isn't on PATH, the target is already inside a repo, or any step
/// fails. We never want a missing git to make a successful scaffold look
/// like it failed.
fn try_git_init(target: &std::path::Path) {
    use std::process::Command;

    // Already inside a repo? Skip.
    let already = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(target)
        .output();
    if let Ok(o) = already {
        if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true" {
            return;
        }
    }

    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(target)
        .status();
    if init.map(|s| !s.success()).unwrap_or(true) {
        return; // git missing or failed; stay silent
    }
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(target)
        .status();
    let _ = Command::new("git")
        .args(["commit", "--quiet", "-m", "riz init"])
        .current_dir(target)
        .status();
    println!("  git init + initial commit");
}

/// Print a "what to do next" block. Specs are now arbitrary (any repo), so we
/// infer the hint from the files that were actually fetched rather than from a
/// known template name.
fn print_next_steps(spec: &str, target: &std::path::Path) {
    let dir = target.display();
    let has = |rel: &str| target.join(rel).exists();
    let rust = has("Cargo.toml");
    let client = has("client/package.json"); // full-stack (Vite) layout

    println!("\n✓ fetched {spec} into {dir}");
    println!("\n  Next steps:");
    println!("    cd {dir}");
    if rust {
        println!("    cargo build --release");
    }
    if client {
        println!("    (cd client && bun install && bun run build)   # build the web client");
    }
    println!("    riz run");
    println!("\n  Then point any MCP client at http://localhost:3000/_riz/mcp");
    println!();
}

/// Connect to a Riz MCP endpoint, run `initialize` + `tools/list`, and print
/// a human-readable report. Intended as a self-validation step before
/// pointing Claude / Cursor / any MCP client at the server — surfaces
/// auth, transport, and tool-registration problems in one place.
/// `riz a2a send <base> <message>` — delegate one task and print the result.
async fn run_a2a_send(base: &str, message: &str, bearer: Option<&str>) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::new();

    // Discover the endpoint from the Agent Card; fall back to the riz path.
    let endpoint = match client
        .get(format!("{base}/.well-known/agent-card.json"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let card: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(name) = card.get("name").and_then(|v| v.as_str()) {
                println!(
                    "agent: {name} — {}",
                    card.get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                );
            }
            card.get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{base}/_riz/a2a"))
        }
        _ => format!("{base}/_riz/a2a"),
    };

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
        "params": { "message": {
            "kind": "message", "role": "user",
            "messageId": uuid::Uuid::new_v4().to_string(),
            "parts": [{ "kind": "text", "text": message }],
        }}
    });
    let mut req = client.post(&endpoint).json(&body);
    if let Some(tok) = bearer {
        req = req.bearer_auth(tok);
    }
    let v: serde_json::Value = req.send().await?.json().await?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        anyhow::bail!(
            "a2a error {}: {}",
            err.get("code").unwrap_or(&serde_json::Value::Null),
            err.get("message").and_then(|v| v.as_str()).unwrap_or("")
        );
    }
    // .pointer(): missing/mistyped fields degrade to the "?" placeholders
    // below instead of panicking on a malformed peer response.
    let state = v
        .pointer("/result/status/state")
        .and_then(|s| s.as_str())
        .unwrap_or("?");
    println!(
        "task:  {}",
        v.pointer("/result/id")
            .and_then(|s| s.as_str())
            .unwrap_or("?")
    );
    println!("state: {state}");
    if let Some(answer) = v
        .pointer("/result/artifacts/0/parts/0/text")
        .and_then(|s| s.as_str())
    {
        println!("\n{answer}");
    }
    if state != "completed" {
        anyhow::bail!("task did not complete (state: {state})");
    }
    Ok(())
}

async fn run_mcp_inspect(url: &str, bearer: Option<&str>) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let init_json = mcp_initialize(&client, url, bearer).await?;
    print_mcp_server_info(url, &init_json);

    // ── tools/list ────────────────────────────────────────────────────────
    let list_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let list_resp = with_bearer(client.post(url).json(&list_body), bearer)
        .send()
        .await?
        .error_for_status()?;
    let list_json: serde_json::Value = list_resp.json().await?;
    if let Some(err) = list_json.get("error") {
        return Err(anyhow::anyhow!("tools/list returned JSON-RPC error: {err}"));
    }

    let tools = list_json
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    println!();
    if tools.is_empty() {
        println!("No tools registered. Add a `[function.<name>]` block to riz.toml.");
        return Ok(());
    }
    println!("Registered tools ({}):", tools.len());
    for tool in &tools {
        print_tool_entry(tool);
    }

    probe_sse_channel(&client, url, bearer).await;

    println!();
    println!("✓ MCP endpoint healthy. Point Claude / Cursor at {url} to use these tools.");
    Ok(())
}

/// Attach the bearer token, when given, to an outgoing inspect request.
fn with_bearer(req: reqwest::RequestBuilder, bearer: Option<&str>) -> reqwest::RequestBuilder {
    if let Some(t) = bearer {
        req.header("authorization", format!("Bearer {t}"))
    } else {
        req
    }
}

/// The MCP `initialize` handshake: POST, decode auth/HTTP/JSON-RPC failures
/// into actionable errors, return the response JSON.
async fn mcp_initialize(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "riz-mcp-inspect", "version": env!("CARGO_PKG_VERSION") }
        }
    });

    let init_resp = with_bearer(client.post(url).json(&init_body), bearer)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to POST {url}: {e}"))?;

    let status = init_resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(anyhow::anyhow!(
            "401 Unauthorized from {url}. The endpoint is bearer-token protected. \
             Pass --bearer <token> or set RIZ_AUTH_BEARER_TOKEN."
        ));
    }
    if !status.is_success() {
        let txt = init_resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("initialize failed: HTTP {status}\n{txt}"));
    }
    let init_json: serde_json::Value = init_resp.json().await?;
    if let Some(err) = init_json.get("error") {
        return Err(anyhow::anyhow!("initialize returned JSON-RPC error: {err}"));
    }
    Ok(init_json)
}

/// Print the server / protocol / capabilities header from the `initialize`
/// result. `.pointer()`: a server that omits (or mistypes) any of these
/// fields gets the "(unknown)" placeholders — never a panic in a diagnostic
/// command.
fn print_mcp_server_info(url: &str, init_json: &serde_json::Value) {
    let protocol_version = init_json
        .pointer("/result/protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let server_name = init_json
        .pointer("/result/serverInfo/name")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let server_version = init_json
        .pointer("/result/serverInfo/version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let caps: Vec<String> = init_json
        .pointer("/result/capabilities")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();

    println!("Connected to {url}");
    println!("  server:        {server_name} {server_version}");
    println!("  protocol:      {protocol_version}");
    println!(
        "  capabilities:  {}",
        if caps.is_empty() {
            "(none)".to_string()
        } else {
            caps.join(", ")
        }
    );
}

/// Print one tool's name/description/schemas from the `tools/list` result.
fn print_tool_entry(tool: &serde_json::Value) {
    let name = tool["name"].as_str().unwrap_or("(unnamed)");
    let desc = tool["description"].as_str().unwrap_or("");
    let has_output_schema = tool.get("outputSchema").is_some();
    println!();
    println!("  • {name}");
    if !desc.is_empty() {
        println!("    {desc}");
    }
    println!(
        "    inputSchema:   {}",
        schema_summary(&tool["inputSchema"])
    );
    if let Some(typed) = typed_params_summary(&tool["inputSchema"]) {
        println!("    typed params:  {typed}");
    }
    if has_output_schema {
        println!(
            "    outputSchema:  {} (MCP 2025-06-18+ structured output)",
            schema_summary(&tool["outputSchema"])
        );
    } else {
        println!("    outputSchema:  — (not declared)");
    }
}

/// SSE channel probe (Streamable HTTP, spec 2025-03-26+): GET with Accept:
/// text/event-stream opens the server-initiated channel. Verify it answers
/// 200 text/event-stream; don't consume the stream.
async fn probe_sse_channel(client: &reqwest::Client, url: &str, bearer: Option<&str>) {
    let sse_resp = with_bearer(
        client.get(url).header("accept", "text/event-stream"),
        bearer,
    )
    .send()
    .await;
    println!();
    match sse_resp {
        Ok(r) if r.status().is_success() => {
            let ct = r
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if ct.contains("text/event-stream") {
                println!("SSE channel:   open (GET → 200 text/event-stream)");
            } else {
                println!("SSE channel:   unexpected content-type {ct:?} on GET");
            }
        }
        Ok(r) => println!("SSE channel:   unavailable (GET → {})", r.status()),
        Err(e) => println!("SSE channel:   probe failed: {e}"),
    }
}

/// Severity of a single doctor check. PASS is silent-ish, WARN means the
/// runtime can still start but something is off, FAIL means startup will
/// almost certainly break.
#[derive(Copy, Clone)]
enum Finding {
    Pass,
    Warn,
    Fail,
}

impl Finding {
    fn glyph(self) -> &'static str {
        match self {
            Finding::Pass => "✓",
            Finding::Warn => "⚠",
            Finding::Fail => "✗",
        }
    }
}

fn report(severity: Finding, label: &str, detail: &str) {
    if detail.is_empty() {
        println!("  {}  {label}", severity.glyph());
    } else {
        println!("  {}  {label}  ·  {detail}", severity.glyph());
    }
}

/// Running warn/fail tally across the doctor checks.
/// saturating: a finding tally can never meaningfully exceed u32::MAX;
/// a pinned count beats an overflow panic mid-diagnosis.
struct DoctorTally {
    warns: u32,
    fails: u32,
}

impl DoctorTally {
    fn record(&mut self, sev: Finding) {
        match sev {
            Finding::Pass => {}
            Finding::Warn => self.warns = self.warns.saturating_add(1),
            Finding::Fail => self.fails = self.fails.saturating_add(1),
        }
    }
}

/// Pre-flight diagnostic. Verifies the environment is ready for `riz run`
/// without actually booting the runtime. Each check prints a single line;
/// the summary tail counts warnings and failures so CI can grep on `riz
/// doctor` output if desired.
async fn run_doctor(config_path: &str) -> anyhow::Result<()> {
    let mut tally = DoctorTally { warns: 0, fails: 0 };

    println!("riz doctor — pre-flight checks\n");

    // 1. riz.toml — exists, parses, validates. Unrecoverable findings
    //    (missing / unparseable config) exit inside.
    let config = doctor_check_config(config_path, &mut tally);

    // 2. Runtime binaries — only check the ones actually needed by the config.
    doctor_check_runtime_binaries(&config, &mut tally);

    // 3. Per-function handler-file presence.
    doctor_check_handler_files(&config, &mut tally);

    // 4. Port availability.
    doctor_check_port(&config, &mut tally).await;

    // 5. Summary.
    let (warns, fails) = (tally.warns, tally.fails);
    println!();
    if fails == 0 && warns == 0 {
        println!("✓ All checks passed. Run `riz run` to start.");
        Ok(())
    } else if fails == 0 {
        println!("⚠ {warns} warning(s). `riz run` will probably work — verify above.");
        Ok(())
    } else {
        println!(
            "✗ {fails} failure(s){}. Fix the items marked ✗ above before `riz run`.",
            if warns > 0 {
                format!(", {warns} warning(s)")
            } else {
                String::new()
            }
        );
        // Rule 1 deviation (docs/SAFETY.md): top-level CLI verdict path;
        // the summary above is the message, the exit code is the contract.
        #[allow(clippy::exit)]
        std::process::exit(1);
    }
}

/// Doctor check 1: riz.toml exists, parses, and validates. A missing or
/// unparseable config is unrecoverable for the remaining checks — those two
/// findings print their own summary and exit 1.
fn doctor_check_config(config_path: &str, tally: &mut DoctorTally) -> config::Config {
    let toml_path = std::path::Path::new(config_path);
    if !toml_path.exists() {
        report(
            Finding::Fail,
            "riz.toml present",
            &format!("not found at {config_path}"),
        );
        tally.record(Finding::Fail);
        println!("\n  Hint: run `riz init <template>` to scaffold one.");
        println!("\n✗ 1 failure — cannot continue without a config.");
        // Rule 1 deviation (docs/SAFETY.md): doctor is a top-level CLI
        // diagnostic whose contract is "summary on stdout, exit code 1" —
        // bubbling an anyhow::Err would duplicate the verdict on stderr.
        #[allow(clippy::exit)]
        std::process::exit(1);
    }
    report(Finding::Pass, "riz.toml present", config_path);
    tally.record(Finding::Pass);

    let config = match config::Config::from_file(config_path) {
        Ok(c) => {
            report(Finding::Pass, "riz.toml parses", "");
            tally.record(Finding::Pass);
            c
        }
        Err(e) => {
            report(Finding::Fail, "riz.toml parses", &format!("{e}"));
            tally.record(Finding::Fail);
            println!(
                "\n✗ {} failure(s) — cannot continue without parseable config.",
                tally.fails
            );
            // Rule 1 deviation (docs/SAFETY.md): top-level CLI verdict path;
            // the summary above is the message, the exit code is the contract.
            #[allow(clippy::exit)]
            std::process::exit(1);
        }
    };

    match config.validate() {
        Ok(_) => {
            report(
                Finding::Pass,
                "riz.toml validates",
                &format!("{} function(s)", config.functions.len()),
            );
            tally.record(Finding::Pass);
        }
        Err(e) => {
            report(Finding::Fail, "riz.toml validates", &e.to_string());
            tally.record(Finding::Fail);
        }
    }
    config
}

/// Doctor check 2: runtime binaries on PATH — only the ones the config's
/// functions actually need.
fn doctor_check_runtime_binaries(config: &config::Config, tally: &mut DoctorTally) {
    let mut needs_bun = false;
    let mut needs_python = false;
    let mut needs_node = false;
    let mut needs_rust_bin: Vec<(String, std::path::PathBuf)> = Vec::new();
    for (name, fc) in &config.functions {
        match fc.runtime {
            config::RuntimeKind::Bun => needs_bun = true,
            config::RuntimeKind::Python => needs_python = true,
            config::RuntimeKind::Node => needs_node = true,
            // Rust and Go handlers are pre-compiled native binaries — there's
            // no run-time toolchain to check, just that the binary exists
            // (verified in the per-function handler-file pass).
            config::RuntimeKind::Rust | config::RuntimeKind::Go => {
                needs_rust_bin.push((name.clone(), fc.handler.clone()));
            }
            // WASM needs no external toolchain at run time — wasmtime is
            // embedded in the riz binary. The `.wasm` module presence is
            // checked in the per-function handler-file pass.
            config::RuntimeKind::Wasm => {}
        }
    }

    if needs_bun {
        match which_binary("bun") {
            Some(path) => {
                report(Finding::Pass, "bun on PATH", &path.display().to_string());
                tally.record(Finding::Pass);
            }
            None => {
                report(
                    Finding::Fail,
                    "bun on PATH",
                    "not found — required for TypeScript/JavaScript handlers",
                );
                tally.record(Finding::Fail);
                println!("       Install: curl -fsSL https://bun.sh/install | bash");
            }
        }
    }

    if needs_python {
        match which_binary("python3") {
            Some(path) => {
                report(
                    Finding::Pass,
                    "python3 on PATH",
                    &path.display().to_string(),
                );
                tally.record(Finding::Pass);
            }
            None => {
                report(
                    Finding::Fail,
                    "python3 on PATH",
                    "not found — required for Python handlers",
                );
                tally.record(Finding::Fail);
            }
        }
    }

    if needs_node {
        match which_binary("node") {
            Some(path) => {
                report(Finding::Pass, "node on PATH", &path.display().to_string());
                tally.record(Finding::Pass);
            }
            None => {
                report(
                    Finding::Fail,
                    "node on PATH",
                    "not found — required for Node.js handlers",
                );
                tally.record(Finding::Fail);
                println!("       Install: https://nodejs.org/en/download");
            }
        }
    }
}

/// Doctor check 3: per-function handler-file presence. For Bun/Python/Node
/// this is the .ts/.py/.mjs file; for Rust it's the precompiled binary at
/// `handler =`; for WASM the `.wasm` module.
fn doctor_check_handler_files(config: &config::Config, tally: &mut DoctorTally) {
    for (name, fc) in &config.functions {
        let handler_str = fc.handler.display().to_string();
        let label = format!("function `{name}` handler");
        match fc.runtime {
            config::RuntimeKind::Bun | config::RuntimeKind::Python | config::RuntimeKind::Node => {
                // Handler is "file.ext.export" or "./path/file.handler". Strip
                // the trailing export segment and check the file exists.
                let candidate = strip_handler_export(&fc.handler);
                if candidate.exists() {
                    report(Finding::Pass, &label, &candidate.display().to_string());
                    tally.record(Finding::Pass);
                } else {
                    report(
                        Finding::Fail,
                        &label,
                        &format!("file not found: {}", candidate.display()),
                    );
                    tally.record(Finding::Fail);
                }
            }
            config::RuntimeKind::Rust | config::RuntimeKind::Go => {
                if fc.handler.exists() {
                    report(Finding::Pass, &label, &handler_str);
                    tally.record(Finding::Pass);
                } else {
                    report(
                        Finding::Warn,
                        &label,
                        &format!("binary not built: {handler_str}"),
                    );
                    tally.record(Finding::Warn);
                    let hint = match fc.runtime {
                        config::RuntimeKind::Go => "go build -o <handler> .",
                        _ => "cargo build --release",
                    };
                    println!("       Hint: {hint}");
                }
            }
            config::RuntimeKind::Wasm => {
                if fc.handler.exists() {
                    report(Finding::Pass, &label, &handler_str);
                    tally.record(Finding::Pass);
                } else {
                    report(
                        Finding::Warn,
                        &label,
                        &format!("wasm module not built: {handler_str}"),
                    );
                    tally.record(Finding::Warn);
                    println!("       Hint: cargo build --release --target wasm32-wasip1");
                }
            }
        }
    }
}

/// Doctor check 4: port availability. If something is already bound to the
/// port, hit /_riz/health to see if it's a healthy Riz — that's still "OK,"
/// just a different kind of OK.
async fn doctor_check_port(config: &config::Config, tally: &mut DoctorTally) {
    let host = config.server.host.clone();
    let port = config.server.port;
    let bind_target = format!("{host}:{port}");
    match std::net::TcpListener::bind(&bind_target) {
        Ok(listener) => {
            drop(listener);
            report(Finding::Pass, "configured port free", &bind_target);
            tally.record(Finding::Pass);
        }
        Err(_) => {
            // Something else is bound. Hit /_riz/health and see if it's riz.
            let probe_url = format!("http://{host}:{port}/_riz/health");
            let probe = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .ok();
            let already_riz = if let Some(c) = probe {
                c.get(&probe_url).send().await.is_ok()
            } else {
                false
            };
            if already_riz {
                report(
                    Finding::Pass,
                    "configured port",
                    &format!("{bind_target} (riz already running)"),
                );
                tally.record(Finding::Pass);
            } else {
                report(
                    Finding::Fail,
                    "configured port free",
                    &format!("{bind_target} is in use by something else"),
                );
                tally.record(Finding::Fail);
            }
        }
    }
}

/// Lookup a binary on PATH. Mirrors `which(1)` — returns the first match.
fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `handler = "src/api/index.handler"` → `src/api/index.ts` (Bun) or
/// `src/api/index.py` (Python). The AWS convention is `file.exportName`;
/// for doctor we just need to verify the file half exists.
///
/// `handler = "./src/api/index.ts"` (explicit path form) → returned as-is.
fn strip_handler_export(handler: &std::path::Path) -> std::path::PathBuf {
    let s = handler.to_string_lossy();
    // Explicit path form: ends in .ts / .js / .mjs / .py / .cjs.
    if s.ends_with(".ts")
        || s.ends_with(".js")
        || s.ends_with(".mjs")
        || s.ends_with(".cjs")
        || s.ends_with(".py")
    {
        return handler.to_path_buf();
    }
    // AWS form: `dir/file.export`. The whole `.export` segment becomes the
    // file extension lookup — try both .ts and .py since we don't know the
    // runtime here without the FunctionConfig. Caller (run_doctor) only
    // calls this for Bun + Python; both look for the file with `.export`
    // stripped + `.ts`/`.py` appended.
    //
    // For Bun, we'll resolve to <name>.ts; for Python, <name>.py. But the
    // caller doesn't differentiate — the simplest correct behavior is to
    // strip the last `.<segment>` and return that as a directory-relative
    // file (without extension). The doctor's check then sees if a file
    // matching the AWS export-segment-stripped path with .ts OR .py exists.
    //
    // Simpler approach: try multiple extensions on the stripped base.
    let parent = handler.parent().unwrap_or_else(|| std::path::Path::new(""));
    let stem = handler.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    for ext in ["ts", "js", "mjs", "py"] {
        let candidate = parent.join(format!("{stem}.{ext}"));
        if candidate.exists() {
            return candidate;
        }
    }
    // No matching file found; return the original so the caller's
    // .exists() check yields a clean "file not found" error message.
    handler.to_path_buf()
}

/// One-line summary of a JSON Schema fragment for the inspect report.
/// One-line summary of the typed path/query params a tool's `inputSchema`
/// declares (v1 roadmap #13) — `path { id: string* }, query { limit: integer }`
/// (`*` = required). None when the tool has only the generic envelope.
fn typed_params_summary(schema: &serde_json::Value) -> Option<String> {
    let section = |key: &str| -> Option<String> {
        // .get() chains: an absent/mistyped section yields None (no summary
        // line) rather than panicking on schemas we don't control.
        let obj = schema.get("properties")?.get(key)?;
        let props = obj.get("properties").and_then(|v| v.as_object())?;
        let required: Vec<&str> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let fields: Vec<String> = props
            .iter()
            .map(|(name, spec)| {
                let kind = spec.get("type").and_then(|v| v.as_str()).unwrap_or("any");
                let star = if required.contains(&name.as_str()) {
                    "*"
                } else {
                    ""
                };
                format!("{name}: {kind}{star}")
            })
            .collect();
        Some(format!(
            "{} {{ {} }}",
            key.trim_end_matches("Params"),
            fields.join(", ")
        ))
    };
    let parts: Vec<String> = ["pathParams", "queryParams"]
        .iter()
        .filter_map(|k| section(k))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn schema_summary(schema: &serde_json::Value) -> String {
    let kind = schema.get("type").and_then(|v| v.as_str()).unwrap_or("any");
    let props: Vec<&str> = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    if props.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} {{ {} }}", props.join(", "))
    }
}

fn effective_config_path(_dev: bool, explicit: Option<&str>) -> String {
    // Always ./riz.toml unless --config is explicit. --dev is a UX flag
    // (TUI + debug logs), not a config-resolution mode. Anywhere outside
    // this repo's `examples/` dir, the old `examples/riz.dev.toml` default
    // was a footgun: silent failure if the file wasn't there, or worse,
    // accidental load of an example config you didn't mean to run.
    explicit
        .map(|s| s.to_string())
        .unwrap_or_else(|| "riz.toml".into())
}

fn effective_log_level(dev: bool, explicit: Option<&str>) -> &str {
    explicit.unwrap_or(if dev { "debug" } else { "info" })
}

fn main() -> anyhow::Result<()> {
    // Intercept the embedded wasmtime host subprocess BEFORE any tokio runtime
    // spins up. `riz __wasm-host <module.wasm> [--dir PATH] [--env K=V]` loads a
    // wasm32-wasip1 module under the WASI capability sandbox and runs its
    // blocking stdin/stdout loop synchronously. This is what the WasmRuntime
    // adapter spawns for each pool worker; keeping it out of the async runtime
    // means each wasm worker stays lean (no multi-threaded scheduler per child).
    let argv: Vec<String> = std::env::args().collect();
    // .get(2..): len >= 2 is guaranteed by the argv[1] match, but a checked
    // slice keeps arg handling panic-free; missing tail args surface as the
    // worker's own usage error.
    if argv.get(1).map(String::as_str) == Some("__wasm-host") {
        return process::wasm::run_host(argv.get(2..).unwrap_or(&[]));
    }
    // The isolated telemetry worker. Like __wasm-host, it runs synchronously
    // *before* any tokio runtime so the child stays lean and decoupled from the
    // host event loop. `riz __telemetry <sink>` reads length-prefixed events
    // from stdin and (2a) appends them as JSON lines to the sink file.
    if argv.get(1).map(String::as_str) == Some("__telemetry") {
        return observability::process::run_worker(argv.get(2..).unwrap_or(&[]));
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

/// Bearer token for the client commands: the --bearer flag, falling back to
/// $RIZ_AUTH_BEARER_TOKEN.
fn resolve_bearer(flag: Option<&String>) -> Option<String> {
    flag.cloned()
        .or_else(|| std::env::var("RIZ_AUTH_BEARER_TOKEN").ok())
}

/// `riz init`, resolved from its CLI flags: `--list` prints the official
/// templates and exits; otherwise a template spec is required.
fn run_init_command(
    spec: Option<&str>,
    dir: Option<&str>,
    reference: Option<&str>,
    list: bool,
    force: bool,
    git: bool,
) -> anyhow::Result<()> {
    if list {
        print_template_list();
        return Ok(());
    }
    let spec = spec.ok_or_else(|| {
        anyhow::anyhow!("template spec required. Run `riz init --list` to see official templates.")
    })?;
    run_init(spec, dir, reference, git, force)
}

/// Dispatch the subcommands that run BEFORE the generic config-load path:
/// `mcp inspect` and `a2a send` talk to a running instance (no config),
/// `doctor` owns its config-load so parse failures surface as findings,
/// `init` must work without an existing config, and `scaffold static`
/// reads the config itself to DERIVE the agent-discovery files.
/// Returns `None` for the serve path (`run`/`validate`/`routes`/default),
/// which `async_main` handles with the loaded config.
async fn dispatch_client_command(cli: &Cli) -> Option<anyhow::Result<()>> {
    match &cli.command {
        Some(Commands::Mcp {
            cmd: McpCmd::Inspect { url, bearer },
        }) => {
            let token = resolve_bearer(bearer.as_ref());
            Some(run_mcp_inspect(url, token.as_deref()).await)
        }
        Some(Commands::A2a {
            cmd:
                A2aCmd::Send {
                    base,
                    message,
                    bearer,
                },
        }) => {
            let token = resolve_bearer(bearer.as_ref());
            Some(run_a2a_send(base, message, token.as_deref()).await)
        }
        Some(Commands::Doctor) => {
            let config_path = effective_config_path(cli.dev, cli.config.as_deref());
            Some(run_doctor(&config_path).await)
        }
        Some(Commands::Init {
            spec,
            dir,
            r#ref,
            list,
            force,
            git,
        }) => Some(run_init_command(
            spec.as_deref(),
            dir.as_deref(),
            r#ref.as_deref(),
            *list,
            *force,
            *git,
        )),
        Some(Commands::Scaffold {
            what:
                ScaffoldCmd::Static {
                    dir,
                    mount,
                    wire,
                    force,
                },
        }) => {
            let config_path = effective_config_path(cli.dev, cli.config.as_deref());
            Some(run_scaffold_static(
                &config_path,
                dir.as_deref(),
                mount,
                *wire,
                *force,
            ))
        }
        _ => None,
    }
}

async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Client / scaffold / diagnostic subcommands run before the generic
    // config-load path below.
    if let Some(result) = dispatch_client_command(&cli).await {
        return result;
    }

    let config_path = effective_config_path(cli.dev, cli.config.as_deref());
    let log_level = effective_log_level(cli.dev, cli.log_level.as_deref());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    // TUI is driven SOLELY by --dev. `riz --dev` turns the TUI on
    // and routes every log into its log panel; plain `riz run` is
    // headless with structured JSON logs on stdout. No --no-tui flag,
    // no TTY auto-detection — one mental model, one switch.
    let tui_enabled = cli.dev;

    // log_tx must exist before we install the tracing subscriber when
    // in TUI mode — TuiLogLayer needs the sink set at registry time.
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<state::LogEntry>(10_000);

    init_tracing(tui_enabled, filter, &log_tx);

    let config = config::Config::from_file(&config_path)?;
    config
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;

    export_broker_resources(&config);

    if let Some(result) = dispatch_config_report(&cli, &config) {
        return result;
    }

    let port = cli.port.unwrap_or(config.server.port);
    let host: std::net::IpAddr = config.server.host.parse()?;
    let addr = SocketAddr::new(host, port);

    let registry = Arc::new(process::runtime::RuntimeRegistry::new()?);
    let cache = cache::CacheLayer::new(&config.cache);

    let (mut telemetry_supervisor, telemetry) = start_telemetry(&config);
    // log_tx / log_rx were already created earlier (before tracing init)
    // so the TUI log layer's sink could be set at registry time. They're
    // in scope from the outer let-binding.

    let deploy_cfg = &config.deploy;
    if config.effective_deploy_key().is_none() && deploy_cfg.allowed_cidrs.is_empty() {
        tracing::error!("SECURITY: /deploy has no auth configured — endpoint will refuse all requests. Set RIZ_DEPLOY_KEY or deploy_key/allowed_cidrs in config.");
    }

    let riz_state = Arc::new(state::RizState::new());
    register_function_states(&riz_state, &config).await;

    let process_manager = Arc::new(process::ProcessManager::new(riz_state.clone()));

    // Spawn one process pool per function. Each spawned process bumps
    // cold_starts on the matching FunctionState.
    process_manager
        .spawn_all(&config.functions, &registry, log_tx.clone())
        .await?;

    let ws_connections = ws::ConnectionStore::new();
    let router =
        build_dispatch_router(&config, &riz_state, &process_manager, &ws_connections).await;

    let app_state = Arc::new(state::AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        router: tokio::sync::RwLock::new(router),
        process_manager,
        cache,
        auth_cache: crate::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections,
    });

    // tui_enabled was determined earlier (before tracing init) so the
    // tracing-subscriber composition matches the actual TUI choice. The
    // value is still in scope from the outer let-binding.
    spawn_tui_or_log_drain(&app_state, tui_enabled);
    spawn_hotreload_watchers(&app_state, &config_path);

    log_startup_mode(cli.dev, addr);

    // Serve until graceful shutdown drains inside `server::run` (its
    // `with_graceful_shutdown`). Once this returns the process is done serving.
    let serve_result = server::run(app_state, addr).await;

    // Post-serve telemetry flush: gracefully shut down the supervisor so every
    // span that was emitted (enqueued) before now is drained to the child and
    // flushed to the sink/exporter — no span loss — within a bounded timeout.
    // The `AppState.telemetry` handle clone may still exist; shutdown drains to
    // empty by deadline rather than waiting for the channel to close, so it
    // never deadlocks on that surviving sender.
    if let Some(sup) = telemetry_supervisor.take() {
        sup.shutdown().await;
    }

    serve_result
}

/// Resource broker: hand the [resources] definitions to wasm pool children
/// through the process environment. Children inherit this plus the DSN env
/// vars the resources name — grants travel per-function in argv (see
/// WasmRuntime::spawn_command); credentials never appear in argv or config.
fn export_broker_resources(config: &config::Config) {
    if config
        .functions
        .values()
        .any(|f| !f.capabilities.is_empty())
    {
        if let Ok(json) = serde_json::to_string(&config.resources) {
            std::env::set_var("RIZ_BROKER_RESOURCES", json);
        }
    }
}

/// The config-report subcommands (`validate` / `routes`): they need the
/// loaded config but no runtime. `None` means the serve path continues.
fn dispatch_config_report(cli: &Cli, config: &config::Config) -> Option<anyhow::Result<()>> {
    match &cli.command {
        Some(Commands::Validate) => {
            println!("Config OK: {} functions", config.functions.len());
            Some(Ok(()))
        }
        Some(Commands::Routes) => {
            print_routes_report(config);
            Some(Ok(()))
        }
        _ => None,
    }
}

/// One startup line, shaped for the mode: human text in --dev, structured
/// fields headless.
fn log_startup_mode(dev: bool, addr: SocketAddr) {
    if dev {
        info!("riz starting in [dev] mode on {addr}");
    } else {
        info!(mode = "production", addr = %addr, "riz starting");
    }
}

/// Install the tracing subscriber matching the TUI choice.
fn init_tracing(
    tui_enabled: bool,
    filter: EnvFilter,
    log_tx: &tokio::sync::mpsc::Sender<state::LogEntry>,
) {
    if tui_enabled {
        // TUI mode: route ALL tracing events into the TUI's log channel.
        // Writing to stdout while the TUI owns the alternate screen
        // corrupts the rendered display (layout "moves," escape
        // sequences leak, scroll breaks).
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tui_log_layer::set_sink(log_tx.clone());
        tracing_subscriber::registry()
            .with(filter)
            .with(tui_log_layer::TuiLogLayer)
            .init();
    } else {
        // Headless: structured JSON for ingestion (Datadog, CloudWatch,
        // Loki, etc.). Same format you'd want piping to `jq`.
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    }
}

/// `riz routes` — two origins, obviously different: your functions, then the
/// system surface riz itself mounts (from the same table the registry and
/// --dev TUI report).
fn print_routes_report(config: &config::Config) {
    println!("user functions:");
    for (name, f) in &config.functions {
        let routes: Vec<String> = f
            .effective_routes(name)
            .into_iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        println!(
            "  {} [{}] {:?}  routes: {}",
            name,
            f.runtime.as_str(),
            f.handler,
            routes.join(", ")
        );
    }
    println!("\nsystem surface (mounted by riz):");
    for (name, routes) in system::system_surface(config) {
        println!("  {} [system]  routes: {}", name, routes.join(", "));
    }
}

/// Telemetry: when enabled, spawn the isolated `__telemetry` child and use
/// its non-blocking handle; otherwise a disabled (drop-everything) handle so
/// every emit call site stays unconditional. When an OTLP endpoint is
/// configured the child exports OTLP/HTTP-JSON; otherwise it appends to a
/// sink file. The supervisor is returned (not leaked) so it can be gracefully
/// shut down — flushing all enqueued spans — after the server drains.
fn start_telemetry(
    config: &config::Config,
) -> (
    Option<observability::TelemetrySupervisor>,
    observability::TelemetryHandle,
) {
    if !config.telemetry.enabled {
        return (None, observability::TelemetryHandle::disabled());
    }
    let sink = std::env::temp_dir().join("riz-telemetry.jsonl");
    let target = observability::ExportTarget {
        endpoint: config.telemetry.endpoint.clone(),
        headers: config.telemetry.headers.clone(),
    };
    match observability::TelemetrySupervisor::spawn(&sink, config.telemetry.queue_capacity, target)
    {
        Ok(sup) => {
            let handle = sup.handle();
            // Keep the supervisor (and its child) alive; shut it down
            // gracefully after the server-run future returns.
            (Some(sup), handle)
        }
        Err(e) => {
            tracing::warn!("telemetry: supervisor spawn failed: {e} — telemetry disabled");
            (None, observability::TelemetryHandle::disabled())
        }
    }
}

/// Register the ENTIRE system surface (probes, /_riz/* admin, and the
/// conditional gateway/A2A endpoints) from the one shared table — the
/// --dev TUI, /_riz/registry, and `riz routes` all report the same truth —
/// then the user functions by name.
async fn register_function_states(riz_state: &Arc<state::RizState>, config: &config::Config) {
    let stage = config.server.stage.clone();
    let default_ttl = config.cache.default_ttl_secs;
    for (name, routes) in system::system_surface(config) {
        riz_state
            .register(state::FunctionState::system(name, routes, &stage))
            .await;
    }
    // Register user functions by name.
    for (name, cfg) in &config.functions {
        riz_state
            .register(state::FunctionState::user(
                name.clone(),
                cfg.clone(),
                &stage,
                default_ttl,
            ))
            .await;
        // Guard pools get their own (system-kind) entries so guard timing
        // surfaces in /_riz/health and metrics without becoming MCP tools.
        if cfg.guard_in.is_some() {
            riz_state
                .register(state::FunctionState::guard(
                    format!("{name}{}", process::guard::GUARD_IN_SUFFIX),
                    &stage,
                ))
                .await;
        }
        if cfg.guard_out.is_some() {
            riz_state
                .register(state::FunctionState::guard(
                    format!("{name}{}", process::guard::GUARD_OUT_SUFFIX),
                    &stage,
                ))
                .await;
        }
    }
}

/// Build the handler list and the dispatch Router, wiring MCP's reentrant
/// dependencies. System handlers mount FIRST so /_riz/* always beats any
/// user attempt to shadow those paths.
async fn build_dispatch_router(
    config: &config::Config,
    riz_state: &Arc<state::RizState>,
    process_manager: &Arc<process::ProcessManager>,
    ws_connections: &ws::ConnectionStore,
) -> router::Router {
    let bearer = config.effective_bearer_token();
    let mcp = Arc::new(system::mcp::McpHandler::new(
        riz_state.clone(),
        bearer.clone(),
    ));
    let mut handlers: Vec<Arc<dyn runtime::LambdaHandler>> = vec![
        Arc::new(system::health::HealthHandler::new(riz_state.clone())),
        Arc::new(system::metrics::MetricsHandler::new(
            riz_state.clone(),
            bearer.clone(),
        )),
        Arc::new(system::registry::RegistryHandler::new(
            riz_state.clone(),
            bearer.clone(),
        )),
        mcp.clone() as Arc<dyn runtime::LambdaHandler>,
        Arc::new(ws::management::ConnectionsHandler::new(
            ws_connections.clone(),
            bearer,
        )),
    ];
    // One ProcessHandler per HTTP function — it declares every route the
    // function serves (including implicit `ANY /<name>` when no routes block
    // is given). WebSocket functions are mounted as axum routes in build_app
    // (see src/server.rs) — they don't go through the LambdaHandler dispatch
    // path.
    for (name, cfg) in &config.functions {
        match cfg.protocol {
            config::Protocol::Http => {
                let h = runtime::process::ProcessHandler::for_function(
                    name,
                    cfg,
                    process_manager.clone(),
                );
                handlers.push(Arc::new(h));
            }
            config::Protocol::WebSocket => {
                // Mounted in build_app; no LambdaHandler instance.
            }
        }
    }
    // McpHandler.tools_call needs an Arc<Router> for reentrant dispatch.
    // We construct the inner Router first, hand it to MCP, then wrap a clone
    // in AppState's RwLock so hot-reload can swap handler lists later.
    let router_arc = Arc::new(router::Router::new(handlers.clone()));
    mcp.set_router(router_arc.clone()).await;
    // Ephemeral WebSocket tool sessions dispatch straight to the process
    // pools (WS functions have no LambdaHandler) and register their
    // collector connections in the same store the management API serves.
    mcp.set_ws_session_deps(system::mcp::WsSessionDeps {
        process_manager: process_manager.clone(),
        connections: ws_connections.clone(),
        stage: config.server.stage.clone(),
    })
    .await;
    router::Router::new(handlers)
}

/// --dev: hand the terminal to the TUI on its own thread. Headless: drain
/// logs to tracing so the bounded channel doesn't back up.
fn spawn_tui_or_log_drain(app_state: &Arc<state::AppState>, tui_enabled: bool) {
    if tui_enabled {
        let tui_state = app_state.clone();
        let tui_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            if let Err(e) = tui::run_tui(tui_state, tui_handle) {
                eprintln!("TUI error: {e}");
            }
        });
    } else {
        let state_for_drain = app_state.clone();
        tokio::spawn(async move {
            let mut rx = state_for_drain.log_rx.lock().await;
            while let Some(entry) = rx.recv().await {
                tracing::debug!(
                    route = entry.route_key.as_deref().unwrap_or("-"),
                    "[{}] {}",
                    entry.level,
                    entry.message
                );
            }
        });
    }
}

/// Watch riz.toml for config reloads, and each function's handler directory
/// for source changes that hot-swap its pool.
fn spawn_hotreload_watchers(app_state: &Arc<state::AppState>, config_path: &str) {
    let watch_state = app_state.clone();
    let watch_config_path = config_path.to_string();
    tokio::spawn(async move {
        hotreload::watch_config(watch_config_path, watch_state).await;
    });

    let handler_watch_state = app_state.clone();
    tokio::spawn(async move {
        hotreload::watch_handler_sources(handler_watch_state).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_flag_parsed() {
        let cli = Cli::try_parse_from(["riz", "--dev"]).unwrap();
        assert!(cli.dev);
        assert!(cli.config.is_none());
        assert!(cli.log_level.is_none());
    }

    #[test]
    fn no_dev_flag_by_default() {
        let cli = Cli::try_parse_from(["riz"]).unwrap();
        assert!(!cli.dev);
    }

    #[test]
    fn explicit_config_overrides_dev_default() {
        let cli = Cli::try_parse_from(["riz", "--dev", "--config", "custom.toml"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("custom.toml"));
        assert_eq!(
            effective_config_path(cli.dev, cli.config.as_deref()),
            "custom.toml"
        );
    }

    #[test]
    fn default_config_is_riz_toml() {
        assert_eq!(effective_config_path(true, None), "riz.toml");
        assert_eq!(effective_config_path(false, None), "riz.toml");
    }

    #[test]
    fn log_level_defaults_by_mode() {
        assert_eq!(effective_log_level(true, None), "debug");
        assert_eq!(effective_log_level(false, None), "info");
        assert_eq!(effective_log_level(true, Some("warn")), "warn");
    }

    /// Malformed / hostile JSON schemas from a remote MCP server must
    /// degrade to placeholders, never panic the inspect command.
    #[test]
    fn schema_summary_degrades_on_malformed_input() {
        use serde_json::json;
        assert_eq!(schema_summary(&json!(null)), "any");
        assert_eq!(schema_summary(&json!("not an object")), "any");
        assert_eq!(schema_summary(&json!({"type": 42})), "any");
        assert_eq!(
            schema_summary(&json!({"type": "object", "properties": "bogus"})),
            "object"
        );
        assert_eq!(
            schema_summary(&json!({"type": "object", "properties": {"a": {}}})),
            "object { a }"
        );
    }

    #[test]
    fn typed_params_summary_degrades_on_malformed_input() {
        use serde_json::json;
        assert_eq!(typed_params_summary(&json!(null)), None);
        assert_eq!(typed_params_summary(&json!({"properties": []})), None);
        assert_eq!(
            typed_params_summary(&json!({"properties": {"pathParams": "bogus"}})),
            None
        );
        // required present but mistyped: fields render without the star.
        let s = typed_params_summary(&json!({
            "properties": {
                "pathParams": {
                    "properties": {"id": {"type": "string"}},
                    "required": "not-an-array"
                }
            }
        }));
        assert_eq!(s.as_deref(), Some("path { id: string }"));
        // well-formed control case
        let s = typed_params_summary(&json!({
            "properties": {
                "pathParams": {
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"]
                }
            }
        }));
        assert_eq!(s.as_deref(), Some("path { id: string* }"));
    }
}
