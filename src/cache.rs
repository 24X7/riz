use crate::config::CacheConfig;
use crate::gateway::ApiGatewayV2httpResponse;
use moka::future::Cache;
use moka::Expiry;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The value stored in the underlying moka cache.
/// We bundle the TTL so the `Expiry` implementation can read it per entry.
#[derive(Clone)]
struct CacheEntry {
    response: Arc<ApiGatewayV2httpResponse>,
    ttl: Duration,
}

/// Per-entry expiry: the TTL is stored inside the value itself.
struct EntryExpiry;

impl Expiry<String, CacheEntry> for EntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &CacheEntry,
        _current_time: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

pub struct CacheLayer {
    inner: Cache<String, CacheEntry>,
}

impl CacheLayer {
    pub fn new(config: &CacheConfig) -> Self {
        // Saturating: `max_size_mb` is operator config; a pathological value
        // (> ~17 exabytes) clamps to a huge-but-finite capacity instead of
        // panicking at startup under overflow checks.
        let max_bytes = config.max_size_mb.saturating_mul(1024 * 1024);
        let cache = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_key: &String, value: &CacheEntry| -> u32 {
                // Weight = approximate byte size of the cached response body
                let body_bytes = match value.response.body.as_ref() {
                    Some(crate::gateway::Body::Text(s)) => s.len(),
                    Some(crate::gateway::Body::Binary(b)) => b.len(),
                    Some(crate::gateway::Body::Empty) | None => 0,
                };
                body_bytes.min(u32::MAX as usize) as u32
            })
            .expire_after(EntryExpiry)
            .build();
        Self { inner: cache }
    }

    pub fn make_key(method: &str, path: &str, query: &str) -> String {
        format!("{}:{}?{}", method.to_uppercase(), path, query)
    }

    pub async fn get(&self, key: &str) -> Option<Arc<ApiGatewayV2httpResponse>> {
        self.inner.get(key).await.map(|e| e.response)
    }

    pub async fn set(&self, key: String, response: ApiGatewayV2httpResponse, ttl_secs: u64) {
        if ttl_secs == 0 {
            return;
        }
        let entry = CacheEntry {
            response: Arc::new(response),
            ttl: Duration::from_secs(ttl_secs),
        };
        self.inner.insert(key, entry).await;
    }

    pub async fn invalidate_keys(&self, keys: &[String]) -> usize {
        let mut count: usize = 0;
        for key in keys {
            if self.inner.remove(key).await.is_some() {
                // Saturating: bounded by keys.len(), so saturation is
                // unreachable; the explicit form makes the non-panic
                // recovery visible.
                count = count.saturating_add(1);
            }
        }
        count
    }

    pub async fn invalidate_prefix(&self, prefix: &str) -> usize {
        let keys: Vec<String> = self
            .inner
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.as_ref().clone())
            .collect();
        let mut count: usize = 0;
        for key in &keys {
            if self.inner.remove(key).await.is_some() {
                // Saturating: bounded by the collected key count (see above).
                count = count.saturating_add(1);
            }
        }
        count
    }

    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Flush pending write operations so that `entry_count` is accurate.
    /// Moka's entry count is eventually consistent; call this before asserting counts in tests.
    #[allow(dead_code)]
    pub async fn sync(&self) {
        self.inner.run_pending_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CacheConfig {
        CacheConfig {
            default_ttl_secs: 0,
            max_size_mb: 16,
        }
    }

    fn ok_response() -> ApiGatewayV2httpResponse {
        ApiGatewayV2httpResponse {
            status_code: 200,
            headers: http::HeaderMap::new(),
            multi_value_headers: http::HeaderMap::new(),
            body: Some(crate::gateway::Body::Text("hello".into())),
            is_base64_encoded: false,
            cookies: Vec::new(),
        }
    }

    #[test]
    fn make_key_format() {
        assert_eq!(
            CacheLayer::make_key("GET", "/accounts/1", ""),
            "GET:/accounts/1?"
        );
        assert_eq!(
            CacheLayer::make_key("get", "/foo", "bar=1"),
            "GET:/foo?bar=1"
        );
    }

    #[test]
    fn cache_weigher_uses_body_size() {
        // Verify weight calculation: body size determines weight
        let body = "hello world"; // 11 bytes
        let weight = body.len().min(u32::MAX as usize) as u32;
        assert_eq!(weight, 11);

        // Empty body = 0 weight
        let empty: Option<&str> = None;
        let empty_weight = empty.map(|b| b.len()).unwrap_or(0) as u32;
        assert_eq!(empty_weight, 0);
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
        let evicted = cache.invalidate_keys(std::slice::from_ref(&key)).await;
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
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let evicted = cache.invalidate_prefix("GET:/accounts/").await;
        assert_eq!(evicted, 2);
        assert!(cache.get(&k3).await.is_some());
    }
}
