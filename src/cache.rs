use std::sync::Arc;
use std::time::{Duration, Instant};
use moka::future::Cache;
use moka::Expiry;
use crate::config::CacheConfig;
use crate::gateway::GatewayResponse;

/// The value stored in the underlying moka cache.
/// We bundle the TTL so the `Expiry` implementation can read it per entry.
#[derive(Clone)]
struct CacheEntry {
    response: Arc<GatewayResponse>,
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
        let max_capacity = (config.max_size_mb * 1024 * 1024 / 512).max(1);
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .expire_after(EntryExpiry)
            .build();
        Self { inner: cache }
    }

    pub fn make_key(method: &str, path: &str, query: &str) -> String {
        format!("{}:{}?{}", method.to_uppercase(), path, query)
    }

    pub async fn get(&self, key: &str) -> Option<Arc<GatewayResponse>> {
        self.inner.get(key).await.map(|e| e.response)
    }

    pub async fn set(&self, key: String, response: GatewayResponse, ttl_secs: u64) {
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
        let mut count = 0;
        for key in keys {
            if self.inner.remove(key).await.is_some() {
                count += 1;
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
        let mut count = 0;
        for key in &keys {
            if self.inner.remove(key).await.is_some() {
                count += 1;
            }
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
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let evicted = cache.invalidate_prefix("GET:/accounts/").await;
        assert_eq!(evicted, 2);
        assert!(cache.get(&k3).await.is_some());
    }
}
