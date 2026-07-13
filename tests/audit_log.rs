//! W1.2 — end-to-end wiring proof for the audit log. Drives a real deploy
//! request through the mounted `/deploy` route and asserts a `riz.audit` event
//! was emitted with the expected fields and no secret material.
//!
//! Capture uses a GLOBAL tracing subscriber (installed once). This is safe
//! because nextest runs every test in its own process, so no other test shares
//! or races this global — and unlike `with_default`, a global subscriber is
//! visible on the axum worker thread that runs the handler.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

#[derive(Clone, Debug)]
struct AuditRecord {
    fields: BTreeMap<String, String>,
}

struct Collector<'a>(&'a mut BTreeMap<String, String>);
impl Visit for Collector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        self.0
            .insert(field.name().to_string(), s.trim_matches('"').to_string());
    }
}

struct CaptureLayer {
    events: Arc<Mutex<Vec<AuditRecord>>>,
}
impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != riz::audit::TARGET {
            return;
        }
        let mut fields = BTreeMap::new();
        event.record(&mut Collector(&mut fields));
        if let Ok(mut g) = self.events.lock() {
            g.push(AuditRecord { fields });
        }
    }
}

#[tokio::test]
async fn deploy_rejection_emits_audit_event() {
    let events = Arc::new(Mutex::new(Vec::<AuditRecord>::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer {
        events: events.clone(),
    });
    // One test per process under nextest → the global installs cleanly.
    tracing::subscriber::set_global_default(subscriber)
        .expect("no other subscriber in this test process");

    // No [deploy] auth configured → deploy_auth_rejection fails closed (503).
    let state = build_state();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = riz::server::build_app(state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/deploy"))
        .json(&serde_json::json!({
            "lambda": "checkout",
            "s3_bucket": "artifacts",
            "s3_key": "checkout.zip"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "no deploy auth configured → fail closed"
    );

    let captured = events.lock().unwrap().clone();
    let deploy_events: Vec<_> = captured
        .iter()
        .filter(|r| r.fields.get("event").map(String::as_str) == Some("deploy"))
        .collect();
    assert_eq!(
        deploy_events.len(),
        1,
        "exactly one deploy audit event (got {captured:?})"
    );
    let f = &deploy_events[0].fields;
    assert_eq!(f.get("outcome").map(String::as_str), Some("rejected"));
    assert_eq!(f.get("lambda").map(String::as_str), Some("checkout"));
    assert_eq!(
        f.get("source").map(String::as_str),
        Some("s3://artifacts/checkout.zip")
    );
    assert!(
        f.contains_key("principal"),
        "deploy audit records the caller principal"
    );
    // The deploy key must never surface in the audit trail.
    let joined = f.values().cloned().collect::<Vec<_>>().join("|");
    assert!(
        !joined.contains("Bearer") && !joined.to_lowercase().contains("deploy_key"),
        "no secret material in audit event: {joined}"
    );
}

fn build_state() -> Arc<riz::state::AppState> {
    let config = riz::config::Config::default();
    let registry = Arc::new(riz::process::runtime::RuntimeRegistry::new().unwrap());
    let cache = riz::cache::CacheLayer::new(&config.cache);
    let telemetry = riz::observability::TelemetryHandle::disabled();
    let riz_state = Arc::new(riz::state::RizState::new());
    let process_manager = Arc::new(riz::process::ProcessManager::new(riz_state.clone()));
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<riz::state::LogEntry>(10_000);
    let rate_limiter = riz::auth::api_key::RateLimiter::from_config(&config.api_keys);
    Arc::new(riz::state::AppState {
        config: tokio::sync::RwLock::new(config),
        router: tokio::sync::RwLock::new(riz::router::Router::new(vec![])),
        process_manager,
        cache,
        auth_cache: riz::auth::authorizer::AuthCache::new(),
        telemetry,
        runtime_registry: registry,
        log_tx,
        log_rx: tokio::sync::Mutex::new(log_rx),
        riz_state,
        ws_connections: riz::ws::ConnectionStore::new(),
        rate_limiter: tokio::sync::RwLock::new(rate_limiter),
    })
}
