use crate::config::DatadogConfig;

pub struct MetricsEmitter {
    enabled: bool,
}

impl MetricsEmitter {
    pub fn new(config: &DatadogConfig) -> Self {
        Self { enabled: config.enabled }
    }

    pub fn record_request(&self, _route: &str, _method: &str, _status: u16, _duration_ms: f64) {}
    pub fn record_cache_hit(&self, _route: &str) {}
    pub fn record_cache_miss(&self, _route: &str) {}
    pub fn record_lambda_crash(&self, _route: &str, _runtime: &str) {}
    pub fn record_lambda_timeout(&self, _route: &str) {}
    pub fn record_lambda_healthy(&self, _route: &str, _healthy: bool) {}
}
