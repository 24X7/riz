//! Authorizer trait, output types, error types, and the TTL-based response cache.

use moka::future::Cache;
use moka::Expiry;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::gateway::ApiGatewayV2httpRequest;

/// Result of a successful authorization check.
#[derive(Clone, Debug)]
pub struct AuthorizerOutput {
    /// Principal identifier (e.g. user ID or service name). Surfaced on
    /// `requestContext.authorizer.lambda["principalId"]` when injected into
    /// the event.
    pub principal_id: String,
    /// Arbitrary key/value context forwarded to the handler via
    /// `requestContext.authorizer.lambda`. Values must be JSON-serialisable.
    pub context: HashMap<String, Value>,
    /// How long this authorization decision should be cached.
    pub ttl: Duration,
}

impl Default for AuthorizerOutput {
    fn default() -> Self {
        Self {
            principal_id: String::new(),
            context: HashMap::new(),
            ttl: Duration::from_secs(300),
        }
    }
}

/// Authorizer error variants.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The request is not authenticated — respond 401.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// The request is authenticated but not permitted — respond 403.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Transient error (network, serialization, process crash) — respond 500.
    #[error("authorizer error: {0}")]
    Other(String),
}

/// Core authorizer contract. All authorizers (REQUEST + JWT) implement this.
#[async_trait::async_trait]
pub trait Authorizer: Send + Sync {
    /// Examine the incoming event and return an authorization decision.
    ///
    /// - `Ok(output)` → authorized. Caller injects `output.context` into
    ///   `requestContext.authorizer.lambda` before invoking the handler.
    /// - `Err(AuthError::Unauthorized)` → 401.
    /// - `Err(AuthError::Forbidden)` → 403.
    /// - `Err(AuthError::Other)` → 500.
    async fn authorize(
        &self,
        event: &ApiGatewayV2httpRequest,
    ) -> Result<AuthorizerOutput, AuthError>;
}

/// Cache key for authorizer responses.
///
/// Keyed by: `(source_ip, auth_header_hash_hex, function_name)`.
/// We hash the Authorization header rather than storing the raw value so
/// the cache key is safe to log and doesn't leak credentials.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthCacheKey {
    source_ip: String,
    auth_header_hash: String,
    function_name: String,
}

impl AuthCacheKey {
    pub fn new(source_ip: &str, auth_header: Option<&str>, function_name: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let hash = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            auth_header.hash(&mut h);
            h.finish()
        };
        Self {
            source_ip: source_ip.to_string(),
            auth_header_hash: format!("{hash:016x}"),
            function_name: function_name.to_string(),
        }
    }

    pub fn to_cache_string(&self) -> String {
        format!(
            "{}|{}|{}",
            self.source_ip, self.auth_header_hash, self.function_name
        )
    }
}

/// Per-entry expiry policy that reads `AuthorizerOutput.ttl`.
struct AuthOutputExpiry;

impl Expiry<String, AuthorizerOutput> for AuthOutputExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &AuthorizerOutput,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }

    fn expire_after_read(
        &self,
        _key: &String,
        _value: &AuthorizerOutput,
        _read_at: Instant,
        duration_until_expiry: Option<Duration>,
        _last_modified_at: Instant,
    ) -> Option<Duration> {
        // Keep existing TTL on read — no sliding-window.
        duration_until_expiry
    }

    fn expire_after_update(
        &self,
        _key: &String,
        value: &AuthorizerOutput,
        _updated_at: Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// Shared authorizer response cache. Backed by `moka::future::Cache` with
/// per-entry TTL derived from `AuthorizerOutput.ttl`.
///
/// Default capacity: 10,000 entries (each entry is small — a few string
/// fields and a HashMap). At 10k entries this stays well within 1 MB.
#[derive(Clone)]
pub struct AuthCache {
    inner: Cache<String, AuthorizerOutput>,
}

impl AuthCache {
    pub fn new() -> Self {
        let cache = Cache::builder()
            .max_capacity(10_000)
            .expire_after(AuthOutputExpiry)
            .build();
        Self { inner: cache }
    }

    pub async fn get(&self, key: &AuthCacheKey) -> Option<AuthorizerOutput> {
        self.inner.get(&key.to_cache_string()).await
    }

    pub async fn insert(&self, key: AuthCacheKey, value: AuthorizerOutput) {
        self.inner.insert(key.to_cache_string(), value).await;
    }

    pub async fn invalidate(&self, key: &AuthCacheKey) {
        self.inner.invalidate(&key.to_cache_string()).await;
    }
}

impl Default for AuthCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_cache_key_deterministic() {
        let k1 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "api");
        let k2 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "api");
        assert_eq!(k1.to_cache_string(), k2.to_cache_string());
    }

    #[test]
    fn auth_cache_key_differs_on_different_header() {
        let k1 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "api");
        let k2 = AuthCacheKey::new("1.2.3.4", Some("Bearer xyz"), "api");
        assert_ne!(k1.to_cache_string(), k2.to_cache_string());
    }

    #[test]
    fn auth_cache_key_differs_on_different_ip() {
        let k1 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "api");
        let k2 = AuthCacheKey::new("5.6.7.8", Some("Bearer abc"), "api");
        assert_ne!(k1.to_cache_string(), k2.to_cache_string());
    }

    #[test]
    fn auth_cache_key_differs_on_different_function() {
        let k1 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "api");
        let k2 = AuthCacheKey::new("1.2.3.4", Some("Bearer abc"), "other");
        assert_ne!(k1.to_cache_string(), k2.to_cache_string());
    }

    #[test]
    fn auth_cache_key_handles_no_auth_header() {
        let k = AuthCacheKey::new("1.2.3.4", None, "api");
        assert!(!k.to_cache_string().is_empty());
    }

    #[tokio::test]
    async fn cache_miss_on_empty() {
        let cache = AuthCache::new();
        let key = AuthCacheKey::new("1.2.3.4", Some("Bearer tok"), "fn");
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn cache_hit_after_insert() {
        let cache = AuthCache::new();
        let key = AuthCacheKey::new("1.2.3.4", Some("Bearer tok"), "fn");
        let output = AuthorizerOutput {
            principal_id: "user123".into(),
            context: HashMap::new(),
            ttl: Duration::from_secs(60),
        };
        cache.insert(key.clone(), output).await;
        let hit = cache.get(&key).await;
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().principal_id, "user123");
    }

    #[tokio::test]
    async fn cache_miss_after_invalidate() {
        let cache = AuthCache::new();
        let key = AuthCacheKey::new("1.2.3.4", Some("Bearer tok"), "fn");
        let output = AuthorizerOutput {
            principal_id: "user123".into(),
            context: HashMap::new(),
            ttl: Duration::from_secs(60),
        };
        cache.insert(key.clone(), output).await;
        cache.invalidate(&key).await;
        assert!(cache.get(&key).await.is_none());
    }
}
