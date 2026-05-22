use cadence::{prelude::*, QueuingMetricSink, StatsdClient, UdpMetricSink};
use std::net::UdpSocket;
use crate::config::DatadogConfig;

pub struct MetricsEmitter {
    client: Option<StatsdClient>,
    env: String,
}

/// Split a "key:value" tag string and call with_tag on the builder.
/// Falls back to with_tag_value if there is no colon.
macro_rules! tag {
    ($builder:expr, $kv:expr) => {{
        let kv: &str = $kv;
        if let Some(pos) = kv.find(':') {
            $builder = $builder.with_tag(&kv[..pos], &kv[pos + 1..]);
        } else {
            $builder = $builder.with_tag_value(kv);
        }
    }};
}

impl MetricsEmitter {
    pub fn new(config: &DatadogConfig) -> Self {
        if !config.enabled {
            return Self { client: None, env: config.env.clone() };
        }
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("metrics: could not bind UDP socket: {e} — metrics disabled");
                return Self { client: None, env: config.env.clone() };
            }
        };
        socket.set_nonblocking(true).ok();
        let sink = match UdpMetricSink::from(config.statsd_host.as_str(), socket) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("metrics: statsd sink error: {e} — metrics disabled");
                return Self { client: None, env: config.env.clone() };
            }
        };
        let queuing_sink = QueuingMetricSink::from(sink);
        let client = StatsdClient::from_sink(&config.service, queuing_sink);
        Self { client: Some(client), env: config.env.clone() }
    }

    pub fn record_request(&self, route: &str, method: &str, status: u16, duration_ms: f64) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let method_tag = format!("method:{}", method.to_lowercase());
            let status_tag = format!("status:{status}");
            let env_tag = format!("env:{}", self.env);

            let mut b = c.time_with_tags("riz.request.duration", duration_ms.round() as u64);
            tag!(b, &route_tag);
            tag!(b, &method_tag);
            tag!(b, &status_tag);
            tag!(b, &env_tag);
            b.send();

            let mut b = c.incr_with_tags("riz.request.count");
            tag!(b, &route_tag);
            tag!(b, &method_tag);
            tag!(b, &status_tag);
            tag!(b, &env_tag);
            b.send();
        }
    }

    pub fn record_cache_hit(&self, route: &str) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let mut b = c.incr_with_tags("riz.cache.hit");
            tag!(b, &route_tag);
            b.send();
        }
    }

    pub fn record_cache_miss(&self, route: &str) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let mut b = c.incr_with_tags("riz.cache.miss");
            tag!(b, &route_tag);
            b.send();
        }
    }

    pub fn record_lambda_crash(&self, route: &str, runtime: &str) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let runtime_tag = format!("runtime:{runtime}");
            let mut b = c.incr_with_tags("riz.lambda.crash");
            tag!(b, &route_tag);
            tag!(b, &runtime_tag);
            b.send();
        }
    }

    pub fn record_lambda_timeout(&self, route: &str) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let mut b = c.incr_with_tags("riz.lambda.timeout");
            tag!(b, &route_tag);
            b.send();
        }
    }

    pub fn record_lambda_healthy(&self, route: &str, healthy: bool) {
        if let Some(c) = &self.client {
            let route_tag = format!("route:{}", sanitize(route));
            let mut b = c.gauge_with_tags("riz.lambda.healthy", if healthy { 1u64 } else { 0u64 });
            tag!(b, &route_tag);
            b.send();
        }
    }
}

fn sanitize(s: &str) -> String {
    s.replace([':', '/', ' '], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_config() -> DatadogConfig {
        DatadogConfig {
            enabled: false,
            statsd_host: "127.0.0.1:8125".into(),
            service: "test".into(),
            env: "test".into(),
        }
    }

    #[test]
    fn disabled_emitter_does_not_panic() {
        let emitter = MetricsEmitter::new(&disabled_config());
        emitter.record_request("GET /foo", "GET", 200, 12.5);
        emitter.record_cache_hit("GET /foo");
        emitter.record_cache_miss("GET /foo");
        emitter.record_lambda_crash("GET /foo", "bun");
        emitter.record_lambda_timeout("GET /foo");
        emitter.record_lambda_healthy("GET /foo", true);
    }

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize("GET /accounts/:id"), "GET__accounts__id");
    }
}
