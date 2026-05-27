//! CORS (Cross-Origin Resource Sharing) policy enforcement.
//!
//! Two entry-points:
//! - `preflight_headers` — build the `Access-Control-Allow-*` headers for an
//!   OPTIONS preflight response.
//! - `response_headers` — build the `Access-Control-Allow-Origin` (+ optional
//!   credentials + expose) headers added to every non-OPTIONS response.
//!
//! The caller (server.rs middleware) is responsible for choosing which to use
//! based on the request method.
//!
//! Defensive handling: invalid Origin values (non-ASCII, containing newlines
//! or other HTTP header-unsafe bytes) are treated as absent — `response_headers`
//! returns an empty map and `origin_allowed` returns false. This prevents HTTP
//! header injection.

use crate::config::CorsConfig;
use http::HeaderMap;

/// Returns true if `origin` is explicitly in the allow-list, or the
/// allow-list contains the wildcard `"*"`.
///
/// An empty allow-list always returns false.
pub fn origin_allowed(origin: &str, allow: &[String]) -> bool {
    allow.iter().any(|a| a == "*" || a == origin)
}

/// Validate that an Origin value is safe to echo back as an HTTP header value.
///
/// Rejects values that:
/// - Contain non-ASCII bytes (would corrupt HTTP headers).
/// - Contain `\r` or `\n` (HTTP header injection vector).
/// - Are empty (meaningless and indicative of a bug).
fn is_safe_origin(origin: &str) -> bool {
    if origin.is_empty() {
        return false;
    }
    // HTTP header values must be ASCII (RFC 7230 §3.2.6).
    if !origin.is_ascii() {
        tracing::debug!(origin, "CORS: rejecting non-ASCII Origin header value");
        return false;
    }
    // Newline characters are an HTTP header injection vector.
    if origin.contains('\r') || origin.contains('\n') {
        tracing::debug!(origin, "CORS: rejecting Origin with newline characters");
        return false;
    }
    true
}

/// Build the preflight (OPTIONS) response headers per the CORS spec.
///
/// Returns an empty `HeaderMap` when the origin is not in the allow-list or
/// when the origin is unsafe — the caller must still return 204, but without
/// CORS headers the browser will treat the preflight as rejected.
pub fn preflight_headers(cfg: &CorsConfig, request_origin: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    if !is_safe_origin(request_origin) {
        tracing::debug!(
            origin = request_origin,
            "CORS preflight: unsafe origin — returning empty headers"
        );
        return h;
    }
    if !origin_allowed(request_origin, &cfg.allow_origins) {
        tracing::debug!(
            origin = request_origin,
            "CORS preflight: origin not in allow-list — returning empty headers"
        );
        return h;
    }
    tracing::debug!(
        origin = request_origin,
        "CORS preflight: origin allowed — building preflight headers"
    );
    // Use `insert` (overwrites any previous value); there is at most one of
    // each CORS header per response.
    h.insert(
        "access-control-allow-origin",
        request_origin
            .parse()
            .expect("origin already validated as ASCII, no newlines"),
    );
    if cfg.allow_credentials {
        h.insert(
            "access-control-allow-credentials",
            "true".parse().expect("static value"),
        );
    }
    if !cfg.allow_methods.is_empty() {
        h.insert(
            "access-control-allow-methods",
            cfg.allow_methods
                .join(", ")
                .parse()
                .unwrap_or_else(|_| "GET".parse().expect("static fallback")),
        );
    }
    if !cfg.allow_headers.is_empty() {
        h.insert(
            "access-control-allow-headers",
            cfg.allow_headers
                .join(", ")
                .parse()
                .unwrap_or_else(|_| "Content-Type".parse().expect("static fallback")),
        );
    }
    if cfg.max_age_secs > 0 {
        h.insert(
            "access-control-max-age",
            cfg.max_age_secs
                .to_string()
                .parse()
                .expect("u64 decimal is valid header value"),
        );
    }
    h
}

/// Build the CORS headers added to non-preflight (non-OPTIONS) responses.
///
/// Returns an empty `HeaderMap` when:
/// - No `Origin` header was present in the request.
/// - The origin value is unsafe (see `is_safe_origin`).
/// - The origin is not in the allow-list.
pub fn response_headers(cfg: &CorsConfig, request_origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    let Some(origin) = request_origin else {
        return h;
    };
    if !is_safe_origin(origin) {
        tracing::debug!(
            origin,
            "CORS response: unsafe origin — skipping CORS headers"
        );
        return h;
    }
    if !origin_allowed(origin, &cfg.allow_origins) {
        tracing::debug!(
            origin,
            "CORS response: origin not in allow-list — skipping CORS headers"
        );
        return h;
    }
    tracing::debug!(
        origin,
        "CORS response: origin allowed — adding CORS headers"
    );
    h.insert(
        "access-control-allow-origin",
        origin
            .parse()
            .expect("origin already validated as ASCII, no newlines"),
    );
    if cfg.allow_credentials {
        h.insert(
            "access-control-allow-credentials",
            "true".parse().expect("static value"),
        );
    }
    if !cfg.expose_headers.is_empty() {
        h.insert(
            "access-control-expose-headers",
            cfg.expose_headers
                .join(", ")
                .parse()
                .unwrap_or_else(|_| "".parse().expect("empty string is valid")),
        );
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CorsConfig;

    fn cfg_with_origins(origins: &[&str]) -> CorsConfig {
        CorsConfig {
            allow_origins: origins.iter().map(|s| s.to_string()).collect(),
            allow_methods: vec!["GET".into(), "POST".into()],
            allow_headers: vec!["Content-Type".into(), "Authorization".into()],
            allow_credentials: false,
            max_age_secs: 3600,
            expose_headers: vec![],
            configured: false,
        }
    }

    // ─── origin_allowed ─────────────────────────────────────────────────────

    #[test]
    fn wildcard_allows_any_origin() {
        assert!(origin_allowed("https://attacker.com", &["*".to_string()]));
        assert!(origin_allowed("https://example.com", &["*".to_string()]));
    }

    #[test]
    fn exact_match_allows_origin() {
        assert!(origin_allowed(
            "https://example.com",
            &["https://example.com".to_string()]
        ));
    }

    #[test]
    fn non_matching_origin_denied() {
        assert!(!origin_allowed(
            "https://attacker.com",
            &["https://example.com".to_string()]
        ));
    }

    #[test]
    fn empty_allow_list_denies_all() {
        assert!(!origin_allowed("https://example.com", &[]));
        assert!(!origin_allowed("*", &[]));
    }

    #[test]
    fn multiple_origins_matches_one_of_them() {
        let allow = vec!["https://a.com".to_string(), "https://b.com".to_string()];
        assert!(origin_allowed("https://a.com", &allow));
        assert!(origin_allowed("https://b.com", &allow));
        assert!(!origin_allowed("https://c.com", &allow));
    }

    // ─── preflight_headers ──────────────────────────────────────────────────

    #[test]
    fn preflight_allowed_origin_returns_required_headers() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = preflight_headers(&cfg, "https://example.com");
        assert_eq!(
            h.get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://example.com")
        );
        assert_eq!(
            h.get("access-control-allow-methods")
                .and_then(|v| v.to_str().ok()),
            Some("GET, POST")
        );
        assert_eq!(
            h.get("access-control-allow-headers")
                .and_then(|v| v.to_str().ok()),
            Some("Content-Type, Authorization")
        );
        assert_eq!(
            h.get("access-control-max-age")
                .and_then(|v| v.to_str().ok()),
            Some("3600")
        );
    }

    #[test]
    fn preflight_denied_origin_returns_empty_headers() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = preflight_headers(&cfg, "https://attacker.com");
        assert!(h.is_empty(), "expected empty headers for denied origin");
    }

    #[test]
    fn preflight_with_credentials_includes_credentials_header() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.allow_credentials = true;
        let h = preflight_headers(&cfg, "https://example.com");
        assert_eq!(
            h.get("access-control-allow-credentials")
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );
    }

    #[test]
    fn preflight_without_credentials_omits_credentials_header() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = preflight_headers(&cfg, "https://example.com");
        assert!(
            h.get("access-control-allow-credentials").is_none(),
            "credentials header must be absent when allow_credentials = false"
        );
    }

    #[test]
    fn preflight_empty_methods_omits_methods_header() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.allow_methods.clear();
        let h = preflight_headers(&cfg, "https://example.com");
        assert!(h.get("access-control-allow-methods").is_none());
    }

    #[test]
    fn preflight_max_age_zero_omits_max_age_header() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.max_age_secs = 0;
        let h = preflight_headers(&cfg, "https://example.com");
        assert!(h.get("access-control-max-age").is_none());
    }

    #[test]
    fn preflight_wildcard_origin_allow_list() {
        let cfg = cfg_with_origins(&["*"]);
        let h = preflight_headers(&cfg, "https://any.com");
        assert_eq!(
            h.get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://any.com")
        );
    }

    #[test]
    fn preflight_rejects_origin_with_newline() {
        let cfg = cfg_with_origins(&["*"]);
        let h = preflight_headers(&cfg, "https://evil.com\r\nX-Injected: true");
        assert!(
            h.is_empty(),
            "origin with newline must produce empty headers"
        );
    }

    #[test]
    fn preflight_rejects_empty_origin() {
        let cfg = cfg_with_origins(&["*"]);
        let h = preflight_headers(&cfg, "");
        assert!(h.is_empty(), "empty origin must produce empty headers");
    }

    // ─── response_headers ───────────────────────────────────────────────────

    #[test]
    fn response_headers_allowed_origin() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = response_headers(&cfg, Some("https://example.com"));
        assert_eq!(
            h.get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://example.com")
        );
    }

    #[test]
    fn response_headers_denied_origin_returns_empty() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = response_headers(&cfg, Some("https://attacker.com"));
        assert!(h.is_empty());
    }

    #[test]
    fn response_headers_none_origin_returns_empty() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = response_headers(&cfg, None);
        assert!(h.is_empty());
    }

    #[test]
    fn response_headers_expose_headers_included() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.expose_headers = vec!["X-Custom".into(), "X-Other".into()];
        let h = response_headers(&cfg, Some("https://example.com"));
        assert_eq!(
            h.get("access-control-expose-headers")
                .and_then(|v| v.to_str().ok()),
            Some("X-Custom, X-Other")
        );
    }

    #[test]
    fn response_headers_no_expose_headers_omits_expose_header() {
        let cfg = cfg_with_origins(&["https://example.com"]);
        let h = response_headers(&cfg, Some("https://example.com"));
        assert!(h.get("access-control-expose-headers").is_none());
    }

    #[test]
    fn response_headers_rejects_newline_in_origin() {
        let cfg = cfg_with_origins(&["*"]);
        let h = response_headers(&cfg, Some("https://evil.com\r\nX-Injected: yes"));
        assert!(h.is_empty(), "origin with newline must be rejected");
    }

    #[test]
    fn response_headers_wildcard_matches_any_safe_origin() {
        let cfg = cfg_with_origins(&["*"]);
        let h = response_headers(&cfg, Some("https://random.example.com"));
        assert!(h.get("access-control-allow-origin").is_some());
    }
}
