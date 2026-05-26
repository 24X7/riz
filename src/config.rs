use indexmap::IndexMap;
use serde::Deserialize;
use std::path::PathBuf;

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
}

fn default_port() -> u16 { 3000 }
fn default_host() -> String { "0.0.0.0".into() }

impl Default for ServerConfig {
    fn default() -> Self { Self { port: default_port(), host: default_host() } }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub default_ttl_secs: u64,
    #[serde(default = "default_cache_size")]
    pub max_size_mb: u64,
}

fn default_cache_size() -> u64 { 128 }

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

fn default_statsd() -> String { "127.0.0.1:8125".into() }
fn default_service() -> String { "riz".into() }
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

/// A user function — one process pool, N routes.
///
/// Mirrors AWS Lambda + API Gateway v2: a Lambda function has a single
/// execution environment that any number of routes can target. The `routes`
/// field lists every (path, method) pair the function answers; if omitted,
/// the default is a single route at `ANY /<function_name>`.
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionConfig {
    pub runtime: RuntimeKind,
    pub handler: PathBuf,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    pub cache_ttl_secs: Option<u64>,
    /// Explicit routes this function serves. If empty, defaults to
    /// `[{ path: "/<name>", method: "ANY" }]`.
    #[serde(default)]
    pub routes: Vec<RouteSpec>,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteSpec {
    pub path: String,
    #[serde(default = "default_method")]
    pub method: String,
}

fn default_method() -> String { "ANY".into() }
fn default_timeout() -> u64 { 30_000 }
fn default_concurrency() -> usize { 1 }

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
        std::env::var("RIZ_DEPLOY_KEY").ok().or_else(|| self.deploy.deploy_key.clone())
    }

    /// Reject configurations that overlap Riz's reserved /_riz/* namespace,
    /// or that use the reserved `_riz` prefix in function names.
    pub fn validate(&self) -> Result<(), String> {
        for (name, func) in &self.functions {
            if name == "_riz" || name.starts_with("_riz") {
                return Err(format!(
                    "function name '{name}' uses reserved '_riz' prefix"
                ));
            }
            for r in func.effective_routes(name) {
                if r.path == "/_riz" || r.path.starts_with("/_riz/") {
                    return Err(format!(
                        "function '{name}' has route path '{}' that uses reserved /_riz/* namespace",
                        r.path
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
    fn parses_function_with_explicit_routes() {
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
        assert_eq!(routes[0].method, "ANY", "method defaults to ANY per AWS convention");
    }

    #[test]
    fn cache_defaults_to_zero_ttl() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.cache.default_ttl_secs, 0);
    }

    #[test]
    fn deploy_key_env_wins() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        std::env::set_var("RIZ_DEPLOY_KEY", "envkey");
        assert_eq!(config.effective_deploy_key(), Some("envkey".into()));
        std::env::remove_var("RIZ_DEPLOY_KEY");
    }

    #[test]
    fn deploy_key_falls_back_to_file() {
        let toml_with_key = r#"
[server]
port = 3000

[deploy]
deploy_key = "filekey"
"#;
        let config: Config = toml::from_str(toml_with_key).unwrap();
        std::env::remove_var("RIZ_DEPLOY_KEY"); // ensure env is clean
        assert_eq!(config.effective_deploy_key(), Some("filekey".into()));
    }

    #[test]
    fn deploy_key_none_when_both_absent() {
        let config: Config = toml::from_str(SAMPLE).unwrap(); // SAMPLE has no deploy_key
        std::env::remove_var("RIZ_DEPLOY_KEY");
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
