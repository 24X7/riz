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
}
