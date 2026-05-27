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
    pub audience: String,
    /// JWKS endpoint URL.
    pub jwks_uri: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    pub bearer_token: Option<String>,
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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
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
    pub auth: AuthConfig,
    /// Global CORS policy. Applied to every user function unless the function
    /// declares its own `[function.<name>.cors]` override.
    #[serde(default)]
    pub cors: CorsConfig,
    /// Function-centric: one entry per function. Each function is a single
    /// process pool serving one or more routes (mirrors AWS Lambda + API GW v2
    /// — one Lambda, N route → function mappings, one execution environment).
    /// TOML reads from `[function.<name>]` (singular per the AWS Lambda
    /// "function" vocabulary); internal field is plural.
    #[serde(default, rename = "function")]
    pub functions: IndexMap<String, FunctionConfig>,
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

fn default_statsd() -> String {
    "127.0.0.1:8125".into()
}
fn default_service() -> String {
    "riz".into()
}
fn default_env() -> String {
    "production".into()
}

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
}

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
        if matches!(self.runtime, RuntimeKind::Rust) {
            // Rust handlers are compiled binaries — the path IS the executable,
            // no module/export split.
            return (self.handler.clone(), String::new());
        }
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
            let runtime_ext = match self.runtime {
                RuntimeKind::Bun => "ts",
                RuntimeKind::Python => "py",
                RuntimeKind::Rust => unreachable!("handled above"),
            };
            return (
                PathBuf::from(format!("{module}.{runtime_ext}")),
                exp.to_string(),
            );
        }
        // Fallback: file path with no extension and no dot — treat as bare module,
        // append runtime extension, default export name "handler".
        let runtime_ext = match self.runtime {
            RuntimeKind::Bun => "ts",
            RuntimeKind::Python => "py",
            RuntimeKind::Rust => unreachable!("handled above"),
        };
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
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Rust => "rust",
            Self::Python => "python",
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
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid config in {}: {e}", path.display()))?;
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
        for (name, func) in &self.functions {
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
                if jwt_cfg.audience.is_empty() {
                    return Err(format!(
                        "function '{name}' JWT authorizer must have a non-empty audience"
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
            concurrency: 1,
            cache_ttl_secs: None,
            routes: vec![],
            cors: None,
            authorizer: None,
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
