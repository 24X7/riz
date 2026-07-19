use indexmap::IndexMap;
use serde::Deserialize;
use std::path::PathBuf;

/// Per-function authorizer configuration.
///
/// Two forms are supported in `riz.toml`:
///
/// ```toml
/// # REQUEST authorizer — name of another function in this config:
/// [function.api]
/// authorizer = "myAuth"
///
/// # Opt-out — skip auth even if a global authorizer is declared:
/// [function.api]
/// authorizer = "none"
///
/// # JWT authorizer — inline block:
/// [function.api.authorizer]
/// type = "jwt"
/// issuer = "https://example.com"
/// audience = "myapp"
/// jwks_uri = "https://example.com/.well-known/jwks.json"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AuthorizerConfig {
    /// A string value: either `"none"` (opt-out) or a function name (REQUEST authorizer).
    FunctionRef(String),
    /// An inline JWT authorizer block.
    Jwt(JwtAuthorizerConfig),
}

/// Inline JWT authorizer configuration block.
#[derive(Debug, Clone, Deserialize)]
pub struct JwtAuthorizerConfig {
    /// Must be `"jwt"`.
    pub r#type: String,
    /// Token issuer URI (validated against `iss` claim).
    pub issuer: String,
    /// Expected audience (validated against `aud` claim).
    ///
    /// Optional. When set (e.g. a WorkOS client id), the `aud` claim is
    /// enforced. When omitted or empty, `aud` validation is SKIPPED — this is
    /// required for IdPs like Clerk whose default session token carries no
    /// `aud` claim. Defaults to empty so existing single-IdP configs that
    /// always set it are unaffected.
    #[serde(default)]
    pub audience: String,
    /// JWKS endpoint URL.
    pub jwks_uri: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    pub bearer_token: Option<String>,
}

/// One per-caller API key (`[api_keys.<name>]`). The map key is the caller's
/// name (for logs/audit); `key` is the secret it presents in the `X-Api-Key`
/// header. `rate_per_sec` is the caller's independent token-bucket ceiling
/// (sustained req/s == burst capacity); absent → identity only, no limit.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyEntry {
    /// The secret presented via the `X-Api-Key` request header.
    pub key: String,
    /// Sustained rate and burst ceiling in requests/second. Absent → unlimited.
    #[serde(default)]
    pub rate_per_sec: Option<u32>,
}

/// CORS policy configuration. Can appear as a top-level `[cors]` block
/// (applies to all user functions) or as a `[function.<name>.cors]` block
/// (overrides the global policy for that function's routes only).
///
/// CORS spec references: MDN Web Docs → CORS.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CorsConfig {
    pub allow_origins: Vec<String>,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub allow_credentials: bool,
    pub max_age_secs: u64,
    pub expose_headers: Vec<String>,
    /// Set internally when a per-function block is parsed; means "explicit
    /// override even if all fields are empty".
    #[serde(skip)]
    pub configured: bool,
}

/// LLM gateway configuration (`[gateway]`).
///
/// ```toml
/// [gateway]
/// default_provider = "mock"
/// fallback_chain = ["mock"]
///
/// [gateway.providers.mock]
/// kind = "mock"
///
/// [gateway.providers.openai]
/// kind = "openai"
/// api_key_env = "OPENAI_API_KEY"
/// base_url = "https://api.openai.com/v1"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GatewayConfig {
    pub default_provider: Option<String>,
    pub fallback_chain: Vec<String>,
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    /// Optional cumulative spend cap (USD). Once reached, the gateway rejects
    /// further requests with HTTP 412.
    pub budget_usd: Option<f64>,
}

impl GatewayConfig {
    /// True when at least one provider is configured.
    pub fn enabled(&self) -> bool {
        !self.providers.is_empty()
    }
}

/// A2A built-in agent configuration (`[agent]`).
///
/// ```toml
/// [agent]
/// name = "shop-support"
/// description = "Answers order questions using the shop's own functions"
/// model = "mock"                    # any gateway model ("anthropic/claude-…")
/// system_prompt = "You are a concise support agent."
/// tools = ["orders", "inventory"]   # allowlist; omit for all HTTP functions
/// max_hops = 5
/// task_timeout_ms = 60000
/// ```
///
/// Spec: docs/superpowers/specs/2026-07-02-a2a-builtin-agent-design.md
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Agent Card identity. Required.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Gateway model the agent reasons with (same routing as `/_riz/v1`).
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Function allowlist the agent may wield as tools. Empty/omitted → every
    /// user function (HTTP tools + WebSocket session tools alike).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Agent-loop cap: model → tool_calls → results counts one hop.
    #[serde(default = "default_agent_max_hops")]
    pub max_hops: u32,
    /// How long SendMessage waits for the task before returning a WORKING
    /// snapshot (the task keeps running; poll GetTask).
    #[serde(default = "default_agent_task_timeout")]
    pub task_timeout_ms: u64,
    /// A2A client side (`[agent.peers]`): name → base URL of another A2A
    /// server. Each peer becomes a `delegate_to_<name>` tool the agent can
    /// wield — the riz-to-riz mesh. Delegations carry a `riz-a2a-hop` header;
    /// an incoming task at or past `max_hops` is rejected (loop protection).
    #[serde(default)]
    pub peers: std::collections::HashMap<String, String>,
}

fn default_agent_max_hops() -> u32 {
    5
}
fn default_agent_task_timeout() -> u64 {
    60_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// Backend kind: "mock" | "openai" | "anthropic" | "ollama".
    pub kind: String,
    /// Env var holding the API key (read at startup; never stored in config).
    /// Consumed by the HTTP providers in `Gateway::from_config`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Override the provider's base URL (e.g. a local Ollama or a proxy).
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    /// Telemetry / observability (`[telemetry]`). Disabled by default. When
    /// enabled, the host runs an isolated `riz __telemetry` child and emits span
    /// events to it through a bounded, non-blocking channel. See
    /// `docs/superpowers/specs/2026-06-10-observability-design.md`.
    /// `enabled`/`queue_capacity` drive the host wiring; `endpoint`/`headers`
    /// drive the OTLP/HTTP-JSON exporter in the isolated child.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub aws: AwsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    /// Per-caller API keys (`[api_keys.<name>]`). Absent/empty → the data
    /// plane is ungated (pre-key behavior preserved). When at least one key is
    /// configured, every non-`/_riz/*` request (function invocations and any
    /// colocated static assets) must present a matching secret in the
    /// `X-Api-Key` header; unknown/absent keys are rejected 401 (fail-closed),
    /// and each key carries its own token-bucket rate limit (429 +
    /// `Retry-After` on exceed). The `/_riz/*` admin/observability plane keeps
    /// its separate `[auth] bearer_token`. Ordered for deterministic
    /// resolution.
    #[serde(default)]
    pub api_keys: IndexMap<String, ApiKeyEntry>,
    /// Global CORS policy. Applied to every user function unless the function
    /// declares its own `[function.<name>.cors]` override.
    #[serde(default)]
    pub cors: CorsConfig,
    /// Prometheus metrics endpoint (`[metrics]`). Enabled by default; set
    /// `enabled = false` to remove `/_riz/metrics`.
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// LLM gateway: provider routing + fallback behind an OpenAI-compatible
    /// endpoint. Absent/empty `[gateway]` → gateway disabled.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// A2A built-in agent (`[agent]`): this instance becomes an
    /// agent2agent-protocol server — an Agent Card at
    /// `/.well-known/agent-card.json` and a JSON-RPC endpoint at `/_riz/a2a`
    /// where peers delegate tasks. The agent reasons through the gateway
    /// (requires `[gateway]`) with this instance's own functions as tools.
    /// Absent → no A2A surface.
    #[serde(default)]
    pub agent: Option<AgentConfig>,
    /// Function-centric: one entry per function. Each function is a single
    /// process pool serving one or more routes (mirrors AWS Lambda + API GW v2
    /// — one Lambda, N route → function mappings, one execution environment).
    /// TOML reads from `[function.<name>]` (singular per the AWS Lambda
    /// "function" vocabulary); internal field is plural.
    #[serde(default, rename = "function")]
    pub functions: IndexMap<String, FunctionConfig>,
    /// Named external resources for the WASM capability broker
    /// (`[resources.pg.<name>]`). Credentials live HERE (host-side, via env
    /// var names) — never in a function block, never across the WASI
    /// boundary. Functions reference resources by name in
    /// `[function.<fn>.capabilities.<grant>]`.
    #[serde(default)]
    pub resources: ResourcesConfig,
    /// Optional static-file mount (`[static]`). Disabled by default. When set,
    /// riz serves files from `dir` as a fallback AFTER function + `/_riz/*`
    /// routes — colocating a site (SPA / landing / the agent-discovery files)
    /// on the same binary and port as the API. See
    /// `docs/superpowers/specs/2026-06-18-static-serving-design.md`.
    #[serde(default, rename = "static")]
    pub static_site: Option<StaticConfig>,
}

/// `[static]` — serve files from a directory as a fallback after API routes.
///
/// Precedence (enforced in `dispatch_lambda`): system endpoints, the LLM
/// gateway, the MCP endpoint, WebSocket upgrades, and every `[function.*]`
/// route win FIRST; static is consulted only when no route owns the path and
/// the request is a GET/HEAD. `/_riz/*` is never served from disk.
#[derive(Debug, Clone, Deserialize)]
pub struct StaticConfig {
    /// Directory served as the site root. Required (its presence enables the
    /// feature); must exist and be a directory at startup.
    pub dir: PathBuf,
    /// URL prefix the dir is served under. Default `/`. Must start with `/`
    /// and must not be `/_riz` or collide with a declared function route.
    #[serde(default = "default_static_mount")]
    pub mount: String,
    /// Directory-index file served for a directory request. Default
    /// `index.html`.
    #[serde(default = "default_static_index")]
    pub index: String,
    /// History-API SPA fallback: an unknown GET that accepts `text/html` and
    /// has no file extension is served `index` (so client-side routes work).
    /// A missing asset (path with an extension) still 404s. Default false.
    #[serde(default)]
    pub spa_fallback: bool,
    /// Optional custom 404 body file (relative to `dir`, e.g. `404.html`).
    /// Empty → a plain `404 not found`.
    #[serde(default)]
    pub not_found: String,
    /// Serve `path.br` / `path.gz` when present and the client's
    /// `Accept-Encoding` allows it. No on-the-fly compression. Default false.
    #[serde(default)]
    pub precompressed: bool,
    /// `Cache-Control` for HTML (index / `*.html`). Default `no-cache` so a
    /// redeploy is picked up immediately.
    #[serde(default = "default_cache_html")]
    pub cache_html: String,
    /// `Cache-Control` for non-hash-named assets. Default 1 hour.
    #[serde(default = "default_cache_assets")]
    pub cache_assets: String,
    /// `Cache-Control` for hash-named assets (e.g. `app.4f1c2a.js`). Default
    /// 1 year immutable.
    #[serde(default = "default_cache_immutable")]
    pub cache_immutable: String,
}

fn default_static_mount() -> String {
    "/".to_string()
}
fn default_static_index() -> String {
    "index.html".to_string()
}
fn default_cache_html() -> String {
    "no-cache".to_string()
}
fn default_cache_assets() -> String {
    "public, max-age=3600".to_string()
}
fn default_cache_immutable() -> String {
    "public, max-age=31536000, immutable".to_string()
}

/// `[resources]` — named backends the broker may reach on behalf of granted
/// functions. Declared once; referenced by `capabilities` grants.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct ResourcesConfig {
    /// `[resources.pg.<name>]` — Postgres-wire backends. One config row
    /// covers Neon, Supabase, RDS, or any self-hosted PG: only the DSN
    /// differs.
    #[serde(default)]
    pub pg: IndexMap<String, PgResourceConfig>,
}

/// One named Postgres backend.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct PgResourceConfig {
    /// Env var holding the connection string (read host-side at spawn; the
    /// DSN itself never appears in config or guest memory).
    pub dsn_env: String,
    /// Server-side statement timeout applied to brokered queries.
    #[serde(default = "default_pg_statement_timeout_ms")]
    pub statement_timeout_ms: u64,
}

fn default_pg_statement_timeout_ms() -> u64 {
    2_000
}

/// `[function.<fn>.capabilities.<grant>]` — one capability grant. The grant
/// NAME (the toml key) is what the guest passes to broker verbs; it is an
/// opaque handle, never a DSN or credential. Deny-by-default: a function
/// with no capabilities block has zero brokered access.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct CapabilityGrant {
    /// Capability class. v1: `"pg"` only.
    pub r#type: String,
    /// Named resource this grant points at, as `<type>.<name>`
    /// (e.g. `"pg.main"` → `[resources.pg.main]`).
    pub resource: String,
    /// `"read-only"` (queries run in a read-only transaction) or
    /// `"read-write"`.
    #[serde(default = "default_grant_mode")]
    pub mode: String,
    /// Concurrency cap: max brokered calls in flight for this grant.
    /// Excess is rejected (`throttled`), never queued — a guest can't
    /// stall the host by piling up calls.
    #[serde(default = "default_grant_max_inflight")]
    pub max_inflight: u32,
    /// Token-bucket rate limit (calls/second). Absent → unlimited.
    #[serde(default)]
    pub rate_per_sec: Option<u32>,
    /// Per-call deadline. The host races the backend I/O against this and
    /// returns `timeout` — the guest invocation is never structurally hung.
    #[serde(default = "default_grant_call_timeout_ms")]
    pub call_timeout_ms: u64,
    /// Request payload cap, enforced before any backend work.
    #[serde(default = "default_grant_max_request_bytes")]
    pub max_request_bytes: usize,
    /// Response payload cap, enforced before bytes are handed to the guest.
    #[serde(default = "default_grant_max_response_bytes")]
    pub max_response_bytes: usize,
}

fn default_grant_mode() -> String {
    "read-write".to_string()
}
fn default_grant_max_inflight() -> u32 {
    4
}
fn default_grant_call_timeout_ms() -> u64 {
    1_500
}
fn default_grant_max_request_bytes() -> usize {
    64 * 1024
}
fn default_grant_max_response_bytes() -> usize {
    1024 * 1024
}

/// Capability classes the broker understands. v1 is Postgres-wire only —
/// the keystone that covers Neon, Supabase, and any PG with zero new code.
pub const CAPABILITY_TYPES: &[&str] = &["pg"];

/// Telemetry / observability config (`[telemetry]`).
///
/// ```toml
/// [telemetry]
/// enabled = false                    # default; disabled => no child, no channel
/// endpoint = "http://localhost:4318" # OTLP/HTTP collector (used in 2b)
/// queue_capacity = 4096              # bounded emit channel
/// [telemetry.headers]                # OTLP export headers (2b)
/// # "x-api-key" = "..."
/// ```
///
/// Every field is `#[serde(default)]` and the struct derives `Default`, so
/// adding it to `Config` is non-breaking.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Master switch. When `false`, the host uses a no-op telemetry handle: no
    /// child process and no channel.
    pub enabled: bool,
    /// OTLP/HTTP collector endpoint. Consumed by the real exporter in phase 2b.
    pub endpoint: Option<String>,
    /// Headers attached to OTLP exports (phase 2b), e.g. auth tokens.
    pub headers: std::collections::BTreeMap<String, String>,
    /// Capacity of the bounded, non-blocking emit channel. Beyond this, events
    /// are dropped rather than blocking the request path.
    pub queue_capacity: usize,
}

fn default_telemetry_queue_capacity() -> usize {
    4096
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: None,
            headers: std::collections::BTreeMap::new(),
            queue_capacity: default_telemetry_queue_capacity(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    /// API Gateway stage name. Surfaced as `requestContext.stage` on every
    /// event. AWS API GW v2 uses `$default` as the implicit deployment stage;
    /// custom stages like `prod` or `v1` go into the request URL (and thus
    /// the path) by convention. Riz mirrors this verbatim.
    #[serde(default = "default_stage")]
    pub stage: String,
}

fn default_port() -> u16 {
    3000
}
fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_stage() -> String {
    "$default".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            stage: default_stage(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Expose `/_riz/metrics` (Prometheus text format). Default true.
    pub enabled: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    #[serde(default)]
    pub default_ttl_secs: u64,
    #[serde(default = "default_cache_size")]
    pub max_size_mb: u64,
}

fn default_cache_size() -> u64 {
    128
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            default_ttl_secs: 0,
            max_size_mb: default_cache_size(),
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

fn default_region() -> String {
    "us-east-1".into()
}

impl Default for AwsConfig {
    fn default() -> Self {
        Self {
            region: default_region(),
        }
    }
}

/// A user function — one process pool, N routes.
///
/// Mirrors AWS Lambda + API Gateway v2: a Lambda function has a single
/// execution environment that any number of routes can target. The `routes`
/// field lists every (path, method) pair the function answers; if omitted,
/// the default is a single route at `ANY /<function_name>`.
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionConfig {
    pub runtime: RuntimeKind,
    #[serde(default)]
    pub protocol: Protocol,
    pub handler: PathBuf,
    /// Handler timeout: how long the spawned process is allowed to take to
    /// produce a response before riz kills it and respawns. Matches AWS
    /// Lambda's per-function `Timeout` setting (max 900 s on AWS, no cap
    /// in riz). Defaults to 30 s.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// API-Gateway-side wait limit: how long the gateway will hold a request
    /// open waiting for the integration. Matches AWS API Gateway v2's
    /// `IntegrationTimeoutInMillis` (max 30 s on AWS HTTP APIs). If the
    /// integration exceeds this, the gateway returns 504 to the client
    /// without killing the handler process (the handler may still complete
    /// and emit its response into the void). Defaults to 30 s.
    #[serde(default = "default_integration_timeout")]
    pub integration_timeout_ms: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    pub cache_ttl_secs: Option<u64>,
    /// Stage variables — surfaced on the event as `stageVariables`. AWS uses
    /// these for per-deployment-stage config that the handler reads at
    /// runtime (e.g. backend URLs, feature flags). Riz makes them per-
    /// function for simplicity.
    #[serde(default)]
    pub stage_variables: std::collections::HashMap<String, String>,
    /// Per-function environment variables injected into the worker process at
    /// spawn — the standard way to hand a handler its `DATABASE_URL` or API
    /// keys without exporting them globally. Mirrors AWS Lambda's
    /// `Environment.Variables`. riz's own variables (`AWS_LAMBDA_*`,
    /// `_HANDLER`, runtime internals) win on conflict. Process runtimes only
    /// (bun/node/python/rust/go); WASM guests keep their deny-by-default WASI
    /// environment — use `stage_variables` or capability grants there.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Explicit routes this function serves. If empty, defaults to
    /// `[{ path: "/<name>", method: "ANY" }]`.
    #[serde(default)]
    pub routes: Vec<RouteSpec>,
    /// Per-function CORS override. When present, overrides the global `[cors]`
    /// block for this function's routes. Absent → global policy applies.
    #[serde(default)]
    pub cors: Option<CorsConfig>,
    /// Optional authorizer for this function.
    ///
    /// - `authorizer = "none"` — skip auth even if a global authorizer exists.
    /// - `authorizer = "myAuth"` — call the named function as a REQUEST authorizer.
    /// - `[function.X.authorizer]` block with `type = "jwt"` — JWT authorizer.
    #[serde(default)]
    pub authorizer: Option<AuthorizerConfig>,
    /// On-box safety: hard cap on the spawned child's virtual address space,
    /// in megabytes. Maps to RLIMIT_AS. Mirrors AWS Lambda's `MemorySize`
    /// setting. Off by default — Bun and Python JITs can spike well above
    /// heap usage, so opt-in only. Linux: strict; macOS: best-effort
    /// (RLIMIT_AS enforcement varies by allocator and JIT mode).
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// On-box safety: hard cap on CPU seconds the spawned child may
    /// accumulate. Maps to RLIMIT_CPU. Exceeding triggers SIGXCPU then
    /// SIGKILL. Off by default — useful for runaway-loop protection on
    /// untrusted handler code.
    #[serde(default)]
    pub cpu_time_secs: Option<u32>,
    /// On-box safety: filesystem allowlist enforced via Linux Landlock
    /// LSM (kernel 5.13+). Each path (and everything beneath it) is
    /// readable/writable by the child; everything else is denied.
    /// Off by default and silently no-op on non-Linux platforms.
    /// Typical pattern: `allowed_paths = ["./handler", "/tmp"]` to
    /// confine the lambda to its own directory + scratch space.
    #[serde(default)]
    pub allowed_paths: Option<Vec<PathBuf>>,
    /// Optional MCP tool tuning — `[function.X.mcp]`. Lets a function refine
    /// the tool surface agents see on `tools/list`: a custom description,
    /// typed query parameters, and a JSON Schema for the request body.
    /// Path params are always typed automatically from the route templates;
    /// this block is for what templates can't express. Absent → the generic
    /// envelope schema (back-compat).
    #[serde(default)]
    pub mcp: Option<McpToolConfig>,
    /// Broker capability grants — `[function.X.capabilities.<grant>]`.
    /// Deny-by-default: absent/empty means the function has zero brokered
    /// access to external resources (today's behavior, unchanged). WASM-only
    /// in v1 (the broker rides the `__wasm-host` boundary).
    #[serde(default)]
    pub capabilities: IndexMap<String, CapabilityGrant>,
    /// Pre-invoke WASM guard — `guard_in = "./guards/validate.wasm"`.
    /// A `wasm32-wasip1` module that sees every incoming event BEFORE the
    /// handler and answers a verdict: allow (optionally with a mutated
    /// event) or deny (status + body, handler never runs). One guard
    /// protects every runtime alike — the guard wraps a Bun, Node, Python,
    /// Rust, or WASM handler identically. Guard failures fail CLOSED.
    #[serde(default)]
    pub guard_in: Option<PathBuf>,
    /// Post-invoke WASM guard — `guard_out = "./guards/redact.wasm"`.
    /// Runs on the response envelope before bytes leave: allow, mutate
    /// (e.g. redact PII), or replace. Same verdict contract as `guard_in`.
    #[serde(default)]
    pub guard_out: Option<PathBuf>,
}

/// `[function.X.mcp]` — per-function MCP tool schema tuning.
///
/// Precise input schemas measurably improve LLM tool-calling accuracy
/// (v1 roadmap #13); this block is how a function declares the parts riz
/// can't infer from the route template alone.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpToolConfig {
    /// Overrides the auto-generated tool description on `tools/list`.
    #[serde(default)]
    pub description: Option<String>,
    /// Typed query parameters: `[function.X.mcp.query.limit]` with
    /// `type` / `description` / `required`. Declared params surface as typed
    /// fields in the tool's `inputSchema.properties.queryParams`; undeclared
    /// params remain accepted (HTTP query strings are open-world).
    #[serde(default)]
    pub query: indexmap::IndexMap<String, McpParamSpec>,
    /// Verbatim JSON Schema for the request body. When present it replaces
    /// the generic `{"type":"string"}` body property so agents send a typed
    /// JSON object; riz serializes it into the Lambda event's string body.
    #[serde(default)]
    pub body: Option<serde_json::Value>,
}

/// One typed parameter inside `[function.X.mcp.query]`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpParamSpec {
    /// JSON Schema scalar type: `string` (default) | `integer` | `number` |
    /// `boolean`. Validated at config load — anything else is a startup error.
    #[serde(rename = "type", default = "default_param_type")]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Required params land in the schema's `required` array and tools/call
    /// rejects requests missing them with JSON-RPC -32602.
    #[serde(default)]
    pub required: bool,
}

fn default_param_type() -> String {
    "string".to_string()
}

/// Scalar types permitted in `McpParamSpec::kind`. Query-string values are
/// strings on the wire, so only types riz can validate from a string form
/// are allowed.
pub const MCP_PARAM_TYPES: &[&str] = &["string", "integer", "number", "boolean"];

impl FunctionConfig {
    /// Effective routes: the declared ones, or the implicit `ANY /<name>`
    /// fallback if no routes block was given.
    pub fn effective_routes(&self, name: &str) -> Vec<RouteSpec> {
        if self.routes.is_empty() {
            vec![RouteSpec {
                path: format!("/{name}"),
                method: default_method(),
            }]
        } else {
            self.routes.clone()
        }
    }

    /// Parse the `handler` field into (module_path, export_name) following
    /// AWS Lambda's `file.export` convention, with Riz extensions for paths
    /// that already include a file extension.
    ///
    /// Forms:
    /// - `"index.handler"` → file `index.<ext>`, export `handler`  (AWS-style)
    /// - `"src/api/index.handler"` → file `src/api/index.<ext>`, export `handler`
    /// - `"./api/index.ts"` → file `./api/index.ts`, export `handler` (default)
    /// - `"./api/index.ts:myFunc"` → file `./api/index.ts`, export `myFunc`
    ///   (Riz-native escape hatch when the file path needs to be explicit)
    ///
    /// Runtime extensions auto-detected by `RuntimeKind`:
    /// - Bun → `.ts`
    /// - Python → `.py`
    /// - Rust → handler is a compiled binary path; export name is meaningless
    ///   and ignored (handler returned verbatim)
    pub fn module_and_export(&self) -> (PathBuf, String) {
        let s = self.handler.to_string_lossy().to_string();
        // One match decides both the early return AND the extension used by
        // every later branch, so no "handled above" invariant needs
        // re-asserting downstream.
        let runtime_ext = match self.runtime {
            // Rust/Go handlers are compiled binaries and WASM handlers are
            // `.wasm` modules — the path IS the artifact, no module/export split.
            RuntimeKind::Rust | RuntimeKind::Go | RuntimeKind::Wasm => {
                return (self.handler.clone(), String::new());
            }
            RuntimeKind::Bun => "ts",
            RuntimeKind::Python => "py",
            RuntimeKind::Node => "mjs",
        };
        // Explicit Riz-native form: "file:exportName"
        if let Some((file, exp)) = s.rsplit_once(':') {
            // But not on Windows where `C:\path` would split — `:` only when
            // it's not preceded by a single drive letter. Conservative: if
            // the part after `:` contains `/` or `\`, it's not an export name.
            if !exp.contains('/') && !exp.contains('\\') {
                return (PathBuf::from(file), exp.to_string());
            }
        }
        // Determine whether the handler already has a known runtime extension.
        let ext = self.handler.extension().and_then(|e| e.to_str());
        let has_known_ext = matches!(ext, Some("ts" | "js" | "mjs" | "cjs" | "py"));
        if has_known_ext {
            // File path already includes the extension — export defaults to "handler"
            // (matches AWS default function name).
            return (self.handler.clone(), "handler".into());
        }
        // AWS-style: last segment after `.` is the export, the rest is the module path.
        if let Some((module, exp)) = s.rsplit_once('.') {
            return (
                PathBuf::from(format!("{module}.{runtime_ext}")),
                exp.to_string(),
            );
        }
        // Fallback: file path with no extension and no dot — treat as bare module,
        // append runtime extension, default export name "handler".
        (
            PathBuf::from(format!("{s}.{runtime_ext}")),
            "handler".into(),
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteSpec {
    pub path: String,
    #[serde(default = "default_method")]
    pub method: String,
}

fn default_method() -> String {
    "ANY".into()
}
fn default_timeout() -> u64 {
    30_000
}
fn default_integration_timeout() -> u64 {
    30_000
}
fn default_concurrency() -> usize {
    1
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Bun,
    Rust,
    Python,
    Node,
    /// A pre-compiled Go binary using the official `aws-lambda-go` SDK against
    /// riz's per-worker AWS Lambda Runtime API (`src/process/runtime_api.rs`;
    /// see `examples/lambdas/echo-go`). Like Rust, the handler IS the
    /// executable — there is no module/export split. Runs via the same
    /// `static_binary` spawner as Rust.
    Go,
    /// A `wasm32-wasip1` module run under wasmtime's WASI capability sandbox
    /// (via the `riz __wasm-host` subprocess). The handler path points at a
    /// `.wasm` file; like Rust, there is no module/export split.
    Wasm,
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Node => "node",
            Self::Go => "go",
            Self::Wasm => "wasm",
        }
    }
}

/// Per-function transport protocol. AWS API Gateway distinguishes HTTP APIs
/// (v2 REST-style) from WebSocket APIs (persistent socket with $connect /
/// $disconnect / $default lifecycle events). Riz functions opt into one or
/// the other.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Protocol {
    #[default]
    Http,
    WebSocket,
}

impl Config {
    pub fn from_file(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            // A missing config is the #1 "it won't start / I'm stuck" moment —
            // so don't just surface a bare ENOENT. Point the user at how to get
            // unstuck (scaffold, --config, --help) instead of a dead end.
            if e.kind() == std::io::ErrorKind::NotFound {
                let cwd = std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|_| ".".to_string());
                anyhow::anyhow!(
                    "no {} found in {cwd}\n\n\
                     riz reads ./riz.toml by default (override with --config <path>).\n\n\
                     Get started:\n  \
                       riz init typescript-http my-app   scaffold a project, then `cd my-app && riz run`\n  \
                       riz init --list                   list available templates\n  \
                       riz --config <path> run           run a riz.toml that lives elsewhere\n\n\
                     Run `riz --help` for all commands.",
                    path.display()
                )
            } else {
                anyhow::anyhow!("cannot read {}: {e}", path.display())
            }
        })?;
        let config: Config = toml::from_str(&text).map_err(|e| {
            // The toml error already carries the line/column + a caret span at
            // the offending token; lead with the file and follow with a pointer
            // to a known-good reference so the user can diff their way out.
            anyhow::anyhow!(
                "invalid riz.toml at {}:\n\n{e}\n\
                 Compare against a working config: `riz init --list` then scaffold one, \
                 or see examples/riz.all.toml. Every field is documented there.",
                path.display()
            )
        })?;
        Ok(config)
    }

    pub fn effective_deploy_key(&self) -> Option<String> {
        std::env::var("RIZ_DEPLOY_KEY")
            .ok()
            .or_else(|| self.deploy.deploy_key.clone())
    }

    /// Effective bearer token: env var `RIZ_AUTH_BEARER_TOKEN` wins over the
    /// `[auth] bearer_token` config field. Returns `None` when neither is set
    /// (open mode — all `/_riz/*` endpoints are public).
    pub fn effective_bearer_token(&self) -> Option<String> {
        std::env::var("RIZ_AUTH_BEARER_TOKEN")
            .ok()
            .or_else(|| self.auth.bearer_token.clone())
    }

    /// Returns the effective CORS policy for the named function. The
    /// per-function `[function.<name>.cors]` block (if present) takes
    /// precedence over the global `[cors]` block.
    ///
    /// The returned config is a clone so callers may cache it cheaply.
    pub fn effective_cors_for(&self, function_name: &str) -> CorsConfig {
        if let Some(func) = self.functions.get(function_name) {
            if let Some(per_fn) = &func.cors {
                return per_fn.clone();
            }
        }
        self.cors.clone()
    }

    /// Reject configurations that overlap Riz's reserved /_riz/* namespace,
    /// use the reserved `_riz` prefix in function names, or declare a runtime
    /// Riz doesn't actually support yet (refuses to start rather than silently
    /// falling back to a different runtime).
    ///
    /// Reserved /_riz/* paths apply ONLY to user functions. System handlers
    /// (HealthHandler, ConnectionsHandler, etc.) mount their routes through
    /// LambdaHandler::routes() and bypass this validation.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(token) = &self.auth.bearer_token {
            if token.is_empty() {
                return Err(
                    "[auth] bearer_token must not be empty — remove the field or supply a non-empty value".into(),
                );
            }
        }
        self.validate_api_keys()?;
        self.validate_agent()?;
        // CORS spec violation (MDN): allow_credentials=true with an empty
        // allow_origins list means no origin will ever be echoed back, so
        // credentials can never flow — almost certainly a misconfiguration.
        // Warn (not error) because the user might be intentionally restricting
        // all origins while debugging.
        if self.cors.allow_credentials && self.cors.allow_origins.is_empty() {
            tracing::warn!(
                "[cors] allow_credentials = true with an empty allow_origins list is a CORS spec \
                 violation (MDN): no Origin will ever be echoed back, so credentials can never \
                 flow. Add at least one origin or set allow_credentials = false."
            );
        }
        Self::validate_cors(&self.cors, "[cors]")?;
        for (name, func) in &self.functions {
            self.validate_function_basics(name, func)?;
            self.validate_function_auth(name, func)?;
            self.validate_function_mcp(name, func)?;
            self.validate_capability_grants(name, func)?;
            if let Some(cors) = &func.cors {
                Self::validate_cors(cors, &format!("[function.{name}.cors]"))?;
            }
        }
        // Never let an unenforced filesystem sandbox be silent: Landlock is
        // Linux-only, so on other platforms `allowed_paths` is ignored and the
        // function runs unconfined. Warn loudly so an operator can't mistake it
        // for confinement (P0.4).
        for name in self.functions_without_enforced_sandbox() {
            tracing::warn!(
                "[function.{name}] allowed_paths is set but this build ({}) has NO filesystem \
                 sandbox — Landlock is Linux-only, so the allowlist is ignored and the function \
                 runs unconfined. Deploy on Linux for filesystem confinement.",
                std::env::consts::OS
            );
        }
        self.validate_gateway()?;
        self.validate_static()?;
        Ok(())
    }

    /// Function names whose `allowed_paths` will NOT be enforced on this
    /// build's platform, because the filesystem allowlist (Landlock) is
    /// Linux-only. Empty on Linux. Drives the loud validation warning that
    /// keeps an unenforced sandbox from being silent (P0.4).
    pub fn functions_without_enforced_sandbox(&self) -> Vec<&str> {
        if crate::process::safety::filesystem_allowlist_enforced() {
            return Vec::new();
        }
        self.functions
            .iter()
            .filter(|(_, f)| f.allowed_paths.is_some())
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Reject `allow_credentials = true` together with a `"*"` wildcard in
    /// `allow_origins`. riz's CORS layer echoes the request Origin (not a
    /// literal `*`) whenever the allow-list matches, so this combination sends
    /// `Access-Control-Allow-Origin: <caller's origin>` *and*
    /// `Access-Control-Allow-Credentials: true` — reflected-origin credentialed
    /// CORS, which lets any site make credentialed cross-origin reads. Browsers
    /// only block the literal-`*`-with-credentials form; reflecting the origin
    /// bypasses that protection, so AWS API Gateway rejects the combination at
    /// config time and riz does the same. (Empty `allow_origins` + credentials
    /// is a separate, non-exploitable misconfiguration, warned about above.)
    fn validate_cors(cors: &CorsConfig, scope: &str) -> Result<(), String> {
        if cors.allow_credentials && cors.allow_origins.iter().any(|o| o == "*") {
            return Err(format!(
                "{scope} allow_credentials = true with a \"*\" wildcard in allow_origins is a \
                 reflected-origin credentialed-CORS vulnerability: because the Origin is echoed \
                 back rather than a literal \"*\", any website could make credentialed \
                 cross-origin requests to your functions. List explicit origins, or set \
                 allow_credentials = false."
            ));
        }
        Ok(())
    }

    /// `[agent]` rides the gateway — an agent with no model plane is a
    /// misconfiguration, not a silent no-op.
    fn validate_agent(&self) -> Result<(), String> {
        let Some(agent) = &self.agent else {
            return Ok(());
        };
        if !self.gateway.enabled() {
            return Err(
                "[agent] requires [gateway]: the built-in agent reasons through the LLM \
                 gateway — add at least one [gateway.providers.*] block"
                    .into(),
            );
        }
        if agent.max_hops == 0 {
            return Err("[agent] max_hops must be >= 1".into());
        }
        for t in &agent.tools {
            if !self.functions.contains_key(t) {
                return Err(format!(
                    "[agent] tools allowlist names unknown function '{t}'"
                ));
            }
        }
        for (peer, url) in &agent.peers {
            if url.trim().is_empty() {
                return Err(format!("[agent.peers] '{peer}' has an empty URL"));
            }
            if !peer
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!(
                    "[agent.peers] name '{peer}' must be alphanumeric/_/- (it becomes the tool name delegate_to_{peer})"
                ));
            }
        }
        Ok(())
    }

    /// `[api_keys]` sanity: no empty secrets, no rate of zero (a key that can
    /// never be used), and no two callers sharing a secret (ambiguous
    /// identity). A bounded, config-fixed set — the rate limiter builds one
    /// bucket per entry (Power-of-10 rule 3).
    fn validate_api_keys(&self) -> Result<(), String> {
        let mut seen_secrets: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (name, entry) in &self.api_keys {
            if entry.key.is_empty() {
                return Err(format!(
                    "[api_keys.{name}] key must not be empty — supply the secret this caller presents in X-Api-Key"
                ));
            }
            if entry.rate_per_sec == Some(0) {
                return Err(format!(
                    "[api_keys.{name}] rate_per_sec must be >= 1 (0 would reject every request) — remove the field for unlimited"
                ));
            }
            if !seen_secrets.insert(entry.key.as_str()) {
                return Err(format!(
                    "[api_keys.{name}] shares its key with another caller — each secret must be unique so the caller resolves unambiguously"
                ));
            }
        }
        Ok(())
    }

    /// Per-function structural checks: reserved names/routes, concurrency,
    /// protocol constraints, and the per-function CORS misconfiguration warning.
    fn validate_function_basics(&self, name: &str, func: &FunctionConfig) -> Result<(), String> {
        // Per-function CORS: same credentials + empty origins check.
        if let Some(fn_cors) = &func.cors {
            if fn_cors.allow_credentials && fn_cors.allow_origins.is_empty() {
                tracing::warn!(
                    "[function.{name}.cors] allow_credentials = true with an empty \
                     allow_origins list is a CORS spec violation (MDN)."
                );
            }
        }
        if name == "_riz" || name.starts_with("_riz") {
            return Err(format!(
                "function name '{name}' uses reserved '_riz' prefix"
            ));
        }
        // concurrency = 0 spawns no worker processes and a 0-permit
        // semaphore, so every invocation 503s forever with no startup
        // error explaining why. Reject it up front.
        if func.concurrency == 0 {
            return Err(format!(
                "function '{name}' has concurrency = 0 — must be at least 1"
            ));
        }
        // All three runtimes (Bun, Python, Rust) are shipped — no validation
        // rejection needed. Adapters live in src/process/{bun,python,rust}.rs
        // and the registry returns the right one per RuntimeKind.
        if matches!(func.protocol, Protocol::WebSocket) {
            // Zero-route WS functions get the implicit ANY /<name> default,
            // which is the upgrade endpoint — that's allowed. Multi-route
            // WS is not: per-message route_selection_expression (multiple
            // handler functions per socket) is not yet supported.
            if func.routes.len() > 1 {
                return Err(format!(
                    "function '{name}' is websocket but declares {} routes; \
                         websocket functions must have at most one route (the upgrade path) — \
                         per-message route_selection_expression is not yet supported",
                    func.routes.len()
                ));
            }
        }
        for r in func.effective_routes(name) {
            if r.path == "/_riz" || r.path.starts_with("/_riz/") {
                return Err(format!(
                    "function '{name}' has route path '{}' that uses reserved /_riz/* namespace",
                    r.path
                ));
            }
        }
        Ok(())
    }

    /// Per-function authorizer checks: function refs must resolve, inline
    /// JWT blocks must be well-formed.
    fn validate_function_auth(&self, name: &str, func: &FunctionConfig) -> Result<(), String> {
        // Validate authorizer references: a FunctionRef that is not "none"
        // must name an existing function in this config.
        if let Some(AuthorizerConfig::FunctionRef(ref auth_name)) = func.authorizer {
            if auth_name != "none" && !self.functions.contains_key(auth_name.as_str()) {
                return Err(format!(
                    "function '{name}' authorizer = \"{auth_name}\" references a function \
                     that does not exist in this config"
                ));
            }
        }
        // Validate JWT authorizer: type must be "jwt".
        if let Some(AuthorizerConfig::Jwt(ref jwt_cfg)) = func.authorizer {
            if jwt_cfg.r#type != "jwt" {
                return Err(format!(
                    "function '{name}' inline authorizer block has type = \"{}\" but only \
                     type = \"jwt\" is supported",
                    jwt_cfg.r#type
                ));
            }
            if jwt_cfg.jwks_uri.is_empty() {
                return Err(format!(
                    "function '{name}' JWT authorizer must have a non-empty jwks_uri"
                ));
            }
            if jwt_cfg.issuer.is_empty() {
                return Err(format!(
                    "function '{name}' JWT authorizer must have a non-empty issuer"
                ));
            }
            // `audience` is intentionally OPTIONAL: when empty, `aud`
            // validation is skipped (required for Clerk's default session
            // token, which has no `aud`). When set, it is enforced in
            // src/auth/jwt.rs (WorkOS and most OAuth IdPs).
        }
        Ok(())
    }

    /// `[function.X.mcp]` — typed tool-schema block. Reject unknown scalar
    /// types and non-object body schemas at startup so a typo surfaces as a
    /// clear config error, not a silently-wrong schema served to agents.
    fn validate_function_mcp(&self, name: &str, func: &FunctionConfig) -> Result<(), String> {
        let Some(mcp) = &func.mcp else {
            return Ok(());
        };
        for (pname, spec) in &mcp.query {
            if !MCP_PARAM_TYPES.contains(&spec.kind.as_str()) {
                return Err(format!(
                    "function '{name}' [function.{name}.mcp.query.{pname}] has \
                     type = \"{}\" — must be one of {MCP_PARAM_TYPES:?}",
                    spec.kind
                ));
            }
        }
        if let Some(body) = &mcp.body {
            if !body.is_object() {
                return Err(format!(
                    "function '{name}' [function.{name}.mcp] body must be a JSON Schema \
                     object (e.g. {{ type = \"object\", ... }})"
                ));
            }
        }
        Ok(())
    }

    /// `[function.X.capabilities.<grant>]` — broker grants. Deny-by-default
    /// means an invalid grant must be a loud startup error, never a
    /// silently-ignored block.
    fn validate_capability_grants(&self, name: &str, func: &FunctionConfig) -> Result<(), String> {
        for (gname, grant) in &func.capabilities {
            if !matches!(func.runtime, RuntimeKind::Wasm) {
                return Err(format!(
                    "function '{name}' grants capability '{gname}' but runtime is \
                     '{}' — broker capabilities are WASM-only in v1 (the broker \
                     rides the __wasm-host sandbox boundary)",
                    func.runtime.as_str()
                ));
            }
            if !CAPABILITY_TYPES.contains(&grant.r#type.as_str()) {
                return Err(format!(
                    "function '{name}' capability '{gname}' has type = \"{}\" — \
                     must be one of {CAPABILITY_TYPES:?}",
                    grant.r#type
                ));
            }
            let Some((rtype, rname)) = grant.resource.split_once('.') else {
                return Err(format!(
                    "function '{name}' capability '{gname}' resource = \"{}\" — \
                     must be \"<type>.<name>\" (e.g. \"pg.main\")",
                    grant.resource
                ));
            };
            if rtype != grant.r#type {
                return Err(format!(
                    "function '{name}' capability '{gname}': resource \"{}\" does not \
                     match type \"{}\"",
                    grant.resource, grant.r#type
                ));
            }
            if rtype == "pg" && !self.resources.pg.contains_key(rname) {
                return Err(format!(
                    "function '{name}' capability '{gname}' references resource \
                     \"{}\" but no [resources.pg.{rname}] block is declared",
                    grant.resource
                ));
            }
            if grant.mode != "read-only" && grant.mode != "read-write" {
                return Err(format!(
                    "function '{name}' capability '{gname}' mode = \"{}\" — must be \
                     \"read-only\" or \"read-write\"",
                    grant.mode
                ));
            }
            if grant.max_inflight == 0 {
                return Err(format!(
                    "function '{name}' capability '{gname}' max_inflight = 0 — every \
                     call would be throttled; must be at least 1"
                ));
            }
            if grant.call_timeout_ms == 0 || grant.call_timeout_ms > func.timeout_ms {
                return Err(format!(
                    "function '{name}' capability '{gname}' call_timeout_ms = {} — must \
                     be 1..={} (the function's timeout_ms)",
                    grant.call_timeout_ms, func.timeout_ms
                ));
            }
        }
        Ok(())
    }

    /// Gateway: validate provider kinds + that default/fallback names exist.
    fn validate_gateway(&self) -> Result<(), String> {
        if !self.gateway.enabled() {
            return Ok(());
        }
        const KNOWN_KINDS: [&str; 4] = ["mock", "openai", "anthropic", "ollama"];
        for (pname, pcfg) in &self.gateway.providers {
            if !KNOWN_KINDS.contains(&pcfg.kind.as_str()) {
                return Err(format!(
                    "[gateway.providers.{pname}] kind = \"{}\" is not one of {KNOWN_KINDS:?}",
                    pcfg.kind
                ));
            }
        }
        if let Some(def) = &self.gateway.default_provider {
            if !self.gateway.providers.contains_key(def) {
                return Err(format!(
                    "[gateway] default_provider = \"{def}\" is not a configured provider"
                ));
            }
        }
        for fb in &self.gateway.fallback_chain {
            if !self.gateway.providers.contains_key(fb) {
                return Err(format!(
                    "[gateway] fallback_chain entry \"{fb}\" is not a configured provider"
                ));
            }
        }
        Ok(())
    }

    /// `[static]` — fail closed at startup so a misconfigured mount never
    /// silently serves nothing (or shadows an API).
    fn validate_static(&self) -> Result<(), String> {
        if let Some(s) = &self.static_site {
            if !s.dir.is_dir() {
                return Err(format!(
                    "[static] dir = {:?} does not exist or is not a directory",
                    s.dir
                ));
            }
            if !s.mount.starts_with('/') {
                return Err(format!(
                    "[static] mount = {:?} must start with '/'",
                    s.mount
                ));
            }
            if s.mount == "/_riz" || s.mount.starts_with("/_riz/") {
                return Err("[static] mount must not use the reserved /_riz namespace".into());
            }
            // The mount must not collide with a declared function route prefix:
            // function routes always win, so a colliding static mount would be
            // dead config — reject it loudly rather than silently shadow.
            for (name, func) in &self.functions {
                for r in func.effective_routes(name) {
                    if s.mount != "/"
                        && (r.path == s.mount
                            || r.path
                                .starts_with(&format!("{}/", s.mount.trim_end_matches('/'))))
                    {
                        return Err(format!(
                            "[static] mount = {:?} collides with function '{name}' route {:?} \
                             — function routes always win, so the static mount would be dead",
                            s.mount, r.path
                        ));
                    }
                }
            }
            // index / not_found must resolve inside dir (no traversal).
            for (label, rel) in [("index", &s.index), ("not_found", &s.not_found)] {
                if rel.is_empty() {
                    continue;
                }
                if rel.contains("..") || rel.starts_with('/') {
                    return Err(format!(
                        "[static] {label} = {rel:?} must be a relative path inside dir (no '..', no leading '/')"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[server]
port = 4000
host = "127.0.0.1"

[function.ping]
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
    fn parses_function() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        let f = config.functions.get("ping").expect("ping function");
        assert_eq!(f.runtime, RuntimeKind::Bun);
        assert_eq!(f.timeout_ms, 1000);
        assert_eq!(f.concurrency, 2);
        // No explicit routes → implicit default ANY /ping
        let routes = f.effective_routes("ping");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/ping");
        assert_eq!(routes[0].method, "ANY");
    }

    #[test]
    fn validate_rejects_zero_concurrency() {
        // concurrency = 0 spawns no processes and a 0-permit semaphore, so every
        // invocation 503s forever with no startup error. Must be rejected.
        let toml_str = r#"
[server]
port = 4000

[function.api]
runtime = "bun"
handler = "./api.ts"
concurrency = 0
"#;
        let config: Config = toml::from_str(toml_str).expect("parses");
        let err = config
            .validate()
            .expect_err("concurrency = 0 must be rejected");
        assert!(
            err.contains("concurrency"),
            "error must mention concurrency; got: {err}"
        );
    }

    #[test]
    fn parses_and_validates_gateway_block() {
        let toml_str = r#"
[server]
port = 3000

[gateway]
default_provider = "mock"
fallback_chain = ["mock"]

[gateway.providers.mock]
kind = "mock"

[function.api]
runtime = "bun"
handler = "./api.ts"
"#;
        let config: Config = toml::from_str(toml_str).expect("parses");
        config.validate().expect("validates");
        assert!(config.gateway.enabled());
        assert_eq!(config.gateway.default_provider.as_deref(), Some("mock"));
        assert_eq!(config.gateway.providers["mock"].kind, "mock");
    }

    #[test]
    fn validate_rejects_unknown_provider_kind() {
        let toml_str = r#"
[server]
port = 3000
[gateway.providers.foo]
kind = "bogus"
"#;
        let config: Config = toml::from_str(toml_str).expect("parses");
        let err = config.validate().unwrap_err();
        assert!(err.contains("kind"), "error must mention kind; got: {err}");
    }

    #[test]
    fn validate_rejects_default_provider_not_configured() {
        let toml_str = r#"
[server]
port = 3000
[gateway]
default_provider = "ghost"
[gateway.providers.mock]
kind = "mock"
"#;
        let config: Config = toml::from_str(toml_str).expect("parses");
        let err = config.validate().unwrap_err();
        assert!(
            err.contains("default_provider"),
            "error must mention default_provider; got: {err}"
        );
    }

    #[test]
    fn function_centric_config_parses() {
        let toml_str = r#"
[server]
port = 8080

[function.api]
runtime = "bun"
handler = "./api.ts"

[[function.api.routes]]
path = "/api/{id}"
method = "GET"

[[function.api.routes]]
path = "/api/{proxy+}"
method = "ANY"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let f = config.functions.get("api").unwrap();
        let routes = f.effective_routes("api");
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/api/{id}");
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[1].path, "/api/{proxy+}");
        assert_eq!(routes[1].method, "ANY");
    }

    #[test]
    fn protocol_defaults_to_http() {
        let toml_str = r#"
[server]
port = 8080

[function.api]
runtime = "bun"
handler = "./api.ts"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.functions.get("api").unwrap().protocol, Protocol::Http);
    }

    #[test]
    fn protocol_parses_websocket() {
        let toml_str = r#"
[server]
port = 8080

[function.chat]
runtime = "bun"
handler = "./chat.ts"
protocol = "websocket"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            c.functions.get("chat").unwrap().protocol,
            Protocol::WebSocket
        );
    }

    #[test]
    fn validate_rejects_websocket_with_multiple_routes() {
        let toml_str = r#"
[server]
port = 8080

[function.chat]
runtime = "bun"
handler = "./chat.ts"
protocol = "websocket"

[[function.chat.routes]]
path = "/chat"
method = "ANY"

[[function.chat.routes]]
path = "/other"
method = "ANY"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("websocket") && err.contains("one route"),
            "got: {err}"
        );
    }

    /// Locks the serde lowercase contract. Without this, a future regression
    /// to `rename_all = "snake_case"` or a permissive parser would silently
    /// broaden the accepted spelling.
    #[test]
    fn protocol_rejects_non_lowercase_spellings() {
        for bad in &["WEBSOCKET", "WebSocket", "Http", "HTTP"] {
            let toml_str = format!(
                r#"
[server]
port = 8080

[function.x]
runtime = "bun"
handler = "./x.ts"
protocol = "{bad}"
"#
            );
            assert!(
                toml::from_str::<Config>(&toml_str).is_err(),
                "protocol = {bad:?} must be rejected (serde rename_all = lowercase)",
            );
        }
    }

    #[test]
    fn route_spec_defaults_method_to_any() {
        let toml_str = r#"
[server]
port = 8080

[function.api]
runtime = "bun"
handler = "./api.ts"

[[function.api.routes]]
path = "/api"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let routes = config.functions.get("api").unwrap().effective_routes("api");
        assert_eq!(
            routes[0].method, "ANY",
            "method defaults to ANY per AWS convention"
        );
    }

    #[test]
    fn cache_defaults_to_zero_ttl() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.cache.default_ttl_secs, 0);
    }

    /// Merged into one test because env vars are process-global and parallel
    /// tests would otherwise race on RIZ_DEPLOY_KEY. Covers all three cases:
    /// env wins over file, file fills in when env absent, None when both absent.
    #[test]
    fn deploy_key_resolution_priority() {
        // 1. env wins
        std::env::set_var("RIZ_DEPLOY_KEY", "envkey");
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.effective_deploy_key(), Some("envkey".into()));

        // 2. file fills in when env absent
        std::env::remove_var("RIZ_DEPLOY_KEY");
        let toml_with_key = r#"
[server]
port = 3000

[deploy]
deploy_key = "filekey"
"#;
        let config: Config = toml::from_str(toml_with_key).unwrap();
        assert_eq!(config.effective_deploy_key(), Some("filekey".into()));

        // 3. None when both absent
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.effective_deploy_key(), None);
    }

    #[test]
    fn cache_config_default_has_correct_max_size() {
        let default = CacheConfig::default();
        assert_eq!(default.max_size_mb, 128);
        assert_eq!(default.default_ttl_secs, 0);
    }

    #[test]
    fn validate_rejects_riz_prefix_path() {
        let toml_str = r#"
[server]
port = 8080

[function.health]
runtime = "bun"
handler = "./h.ts"

[[function.health.routes]]
path = "/_riz/health"
method = "GET"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(err.contains("/_riz"));
    }

    #[test]
    fn validate_rejects_bare_riz_path() {
        let toml_str = r#"
[server]
port = 8080

[function.x]
runtime = "bun"
handler = "./h.ts"

[[function.x.routes]]
path = "/_riz"
method = "GET"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn allowed_paths_sandbox_enforcement_matches_platform() {
        // A function requesting filesystem confinement.
        let toml_str = r#"
[server]
port = 8080

[function.sandboxed]
runtime = "bun"
handler = "./h.ts"
allowed_paths = ["/tmp"]
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        // Config still validates on every platform — the warning is non-fatal.
        assert!(c.validate().is_ok());

        let flagged = c.functions_without_enforced_sandbox();
        if cfg!(target_os = "linux") {
            assert!(
                flagged.is_empty(),
                "Landlock enforces allowed_paths on Linux — nothing flagged"
            );
        } else {
            assert_eq!(
                flagged,
                vec!["sandboxed"],
                "off Linux the unenforced sandbox must be flagged, not silent"
            );
        }
    }

    #[test]
    fn no_allowed_paths_is_never_flagged() {
        let toml_str = r#"
[server]
port = 8080

[function.plain]
runtime = "bun"
handler = "./h.ts"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(
            c.functions_without_enforced_sandbox().is_empty(),
            "a function without allowed_paths is never flagged"
        );
    }

    #[test]
    fn validate_rejects_wildcard_origin_with_credentials() {
        // Reflected-origin credentialed CORS: `*` + credentials lets any site
        // make credentialed cross-origin reads. Must be a hard error.
        let toml_str = r#"
[server]
port = 8080

[cors]
allow_origins = ["*"]
allow_credentials = true
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(err.contains("[cors]"), "err names the scope: {err}");
        assert!(
            err.contains("credentialed") || err.contains("allow_credentials"),
            "err explains the vulnerability: {err}"
        );
    }

    #[test]
    fn malformed_toml_error_names_file_and_points_at_a_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("riz.toml");
        // `port` should be an integer — a string is a type error toml reports
        // with a line/column caret.
        std::fs::write(&path, "[server]\nport = \"not-a-number\"\n").unwrap();
        let err = Config::from_file(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid riz.toml"),
            "leads with the problem: {msg}"
        );
        assert!(msg.contains("riz.toml"), "names the file: {msg}");
        assert!(
            msg.contains("riz init --list") || msg.contains("riz.all.toml"),
            "points at a known-good reference: {msg}"
        );
    }

    #[test]
    fn api_keys_parse_and_validate() {
        let toml_str = r#"
[api_keys.alice]
key = "secret-alice"
rate_per_sec = 100

[api_keys.svc]
key = "secret-svc"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.api_keys.len(), 2);
        assert_eq!(c.api_keys["alice"].rate_per_sec, Some(100));
        assert_eq!(c.api_keys["svc"].rate_per_sec, None, "absent → unlimited");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn api_keys_reject_empty_secret() {
        let toml_str = r#"
[api_keys.alice]
key = ""
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("[api_keys.alice]"),
            "err names the caller: {err}"
        );
        assert!(err.contains("empty"), "err explains: {err}");
    }

    #[test]
    fn api_keys_reject_zero_rate() {
        let toml_str = r#"
[api_keys.alice]
key = "s"
rate_per_sec = 0
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(err.contains("rate_per_sec"), "err names the field: {err}");
    }

    #[test]
    fn api_keys_reject_duplicate_secrets() {
        // Two callers sharing a secret would resolve ambiguously.
        let toml_str = r#"
[api_keys.alice]
key = "shared"

[api_keys.bob]
key = "shared"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("shares its key"),
            "err explains the clash: {err}"
        );
    }

    #[test]
    fn validate_accepts_wildcard_origin_without_credentials() {
        let toml_str = r#"
[server]
port = 8080

[cors]
allow_origins = ["*"]
allow_credentials = false
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.validate().is_ok(), "wildcard alone is fine");
    }

    #[test]
    fn validate_accepts_explicit_origins_with_credentials() {
        let toml_str = r#"
[server]
port = 8080

[cors]
allow_origins = ["https://app.example.com"]
allow_credentials = true
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(
            c.validate().is_ok(),
            "explicit origin + credentials is fine"
        );
    }

    #[test]
    fn validate_rejects_wildcard_credentials_in_per_function_cors() {
        // The per-function override must be validated too, not just [cors].
        let toml_str = r#"
[server]
port = 8080

[function.api]
runtime = "bun"
handler = "./h.ts"

[function.api.cors]
allow_origins = ["*"]
allow_credentials = true
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("[function.api.cors]"),
            "err names the per-function scope: {err}"
        );
    }

    #[test]
    fn validate_rejects_riz_prefix_function_name() {
        let toml_str = r#"
[server]
port = 8080

[function._riz]
runtime = "bun"
handler = "./h.ts"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let err = c.validate().unwrap_err();
        assert!(err.contains("_riz"));
    }

    fn fc(runtime: RuntimeKind, handler: &str) -> FunctionConfig {
        FunctionConfig {
            runtime,
            protocol: Default::default(),
            handler: PathBuf::from(handler),
            timeout_ms: 1000,
            integration_timeout_ms: 30000,
            stage_variables: Default::default(),
            env: Default::default(),
            concurrency: 1,
            cache_ttl_secs: None,
            routes: vec![],
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

    #[test]
    fn handler_export_syntax_resolves() {
        let c = fc(RuntimeKind::Bun, "src/api/index.handler");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("src/api/index.ts"));
        assert_eq!(export, "handler");
    }

    #[test]
    fn handler_aws_style_with_custom_export_name() {
        let c = fc(RuntimeKind::Bun, "src/api/index.myHandler");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("src/api/index.ts"));
        assert_eq!(export, "myHandler");
    }

    #[test]
    fn handler_with_explicit_ts_extension_keeps_default_handler_export() {
        let c = fc(RuntimeKind::Bun, "./examples/api/index.ts");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("./examples/api/index.ts"));
        assert_eq!(export, "handler");
    }

    #[test]
    fn handler_with_explicit_extension_and_riz_colon_export_override() {
        let c = fc(RuntimeKind::Bun, "./examples/api/index.ts:myFunc");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("./examples/api/index.ts"));
        assert_eq!(export, "myFunc");
    }

    #[test]
    fn handler_python_aws_style() {
        let c = fc(RuntimeKind::Python, "app.lambda_handler");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("app.py"));
        assert_eq!(export, "lambda_handler");
    }

    #[test]
    fn handler_rust_returns_handler_path_verbatim() {
        let c = fc(RuntimeKind::Rust, "./target/release/my-handler");
        let (module, export) = c.module_and_export();
        assert_eq!(module, PathBuf::from("./target/release/my-handler"));
        assert_eq!(export, "");
    }

    #[test]
    fn implicit_default_route_uses_function_name() {
        let toml_str = r#"
[server]
port = 8080

[function.users]
runtime = "bun"
handler = "./users.ts"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        let routes = c.functions.get("users").unwrap().effective_routes("users");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].method, "ANY");
    }

    #[test]
    fn validate_accepts_normal_routes() {
        let c: Config = toml::from_str(SAMPLE).unwrap();
        assert!(c.validate().is_ok());
    }
}
