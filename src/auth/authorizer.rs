//! Authorizer trait, output types, error types, and the TTL-based response cache.

use moka::future::Cache;
use moka::Expiry;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::auth::jwt::JwtAuthorizer;
use crate::config::JwtAuthorizerConfig;
use crate::gateway::ApiGatewayV2httpRequest;

/// A JWKS is re-fetched at most once per this window per `jwks_uri`. Between
/// fetches, cache-missed requests reuse the constructed authorizer, so a burst
/// of distinct invalid tokens cannot amplify into one IdP fetch each.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(300);

/// Distinct `jwks_uri`s a deployment authorizes against — small; bounds the
/// authorizer cache (rule 3).
const JWKS_AUTHORIZER_CAPACITY: u64 = 64;

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
///
/// The hash is SHA-256, not `DefaultHasher`: this key gates a cached ALLOW
/// decision, so a collision between two different credentials would be an
/// authorization bypass. A fixed-key 64-bit SipHash is not collision-
/// resistant against an adversary; a 256-bit cryptographic digest is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthCacheKey {
    source_ip: String,
    auth_header_hash: String,
    function_name: String,
}

impl AuthCacheKey {
    pub fn new(source_ip: &str, auth_header: Option<&str>, function_name: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        // Domain-separate "no header" from any literal header value.
        match auth_header {
            Some(h) => {
                hasher.update([1u8]);
                hasher.update(h.as_bytes());
            }
            None => hasher.update([0u8]),
        }
        Self {
            source_ip: source_ip.to_string(),
            auth_header_hash: format!("{:x}", hasher.finalize()),
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
    /// JWKS-backed authorizers keyed by `jwks_uri`. Each holds one fetched
    /// JWKS; entries expire after [`JWKS_REFRESH_COOLDOWN`]. This is what stops
    /// a burst of decision-cache-missed requests (a stream of distinct invalid
    /// Bearer tokens, say) from constructing a fresh authorizer — and thus
    /// firing a fresh JWKS fetch at the IdP — on every request.
    jwks_authorizers: Cache<String, Arc<JwtAuthorizer>>,
}

impl AuthCache {
    pub fn new() -> Self {
        let cache = Cache::builder()
            .max_capacity(10_000)
            .expire_after(AuthOutputExpiry)
            .build();
        let jwks_authorizers = Cache::builder()
            .max_capacity(JWKS_AUTHORIZER_CAPACITY)
            .time_to_live(JWKS_REFRESH_COOLDOWN)
            .build();
        Self {
            inner: cache,
            jwks_authorizers,
        }
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

    /// Return a `JwtAuthorizer` for `cfg.jwks_uri`, constructing it — and
    /// fetching the JWKS — at most once per [`JWKS_REFRESH_COOLDOWN`] per uri.
    /// Concurrent misses single-flight via moka's `try_get_with`, so a burst of
    /// invalid tokens triggers exactly one fetch. Fail-closed: a build error
    /// (unreachable/invalid JWKS) is returned and nothing is cached, so the
    /// next request retries rather than serving a stale authorizer.
    pub async fn jwt_authorizer(
        &self,
        cfg: &JwtAuthorizerConfig,
    ) -> Result<Arc<JwtAuthorizer>, AuthError> {
        let cfg = cfg.clone();
        self.jwks_authorizers
            .try_get_with(cfg.jwks_uri.clone(), async move {
                JwtAuthorizer::new(cfg).await.map(Arc::new)
            })
            .await
            .map_err(|e| AuthError::Other(e.to_string()))
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

    #[test]
    fn auth_cache_key_none_differs_from_empty_header() {
        // Domain separation: an absent Authorization header must never alias
        // an empty one (or any literal value).
        let k1 = AuthCacheKey::new("1.2.3.4", None, "api");
        let k2 = AuthCacheKey::new("1.2.3.4", Some(""), "api");
        assert_ne!(k1.to_cache_string(), k2.to_cache_string());
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
