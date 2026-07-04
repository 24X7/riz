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
use http::{HeaderMap, HeaderValue};

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

/// Validate the request Origin and parse it into a typed `HeaderValue`,
/// applying the allow-list. Returns `None` — the module's documented
/// "treat as absent" semantic — when the origin is unsafe, not in the
/// allow-list, or not a legal HTTP header value.
///
/// The final `HeaderValue::from_str` is the authority on echoability:
/// `is_safe_origin` admits ASCII control characters other than CR/LF (e.g.
/// NUL), which are not legal header bytes. Parsing here means the insert
/// sites hold an already-proven value — no panic on any input (rule 7).
fn allowed_origin_value(cfg: &CorsConfig, origin: &str) -> Option<HeaderValue> {
    if !is_safe_origin(origin) {
        tracing::debug!(origin, "CORS: unsafe origin — treating as absent");
        return None;
    }
    if !origin_allowed(origin, &cfg.allow_origins) {
        tracing::debug!(
            origin,
            "CORS: origin not in allow-list — treating as absent"
        );
        return None;
    }
    match HeaderValue::from_str(origin) {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::debug!(
                origin,
                "CORS: origin is not a valid header value — treating as absent"
            );
            None
        }
    }
}

/// Join an operator-configured list ("GET", "POST" → "GET, POST") into a
/// header value. Config is operator input, not remote input, but it must
/// still never panic the request task (rule 7): a value the `http` crate
/// rejects (e.g. an embedded newline) degrades to `fallback` with a warning,
/// and the malformed text can never smuggle an injected header.
fn joined_header_value(parts: &[String], header_name: &str, fallback: HeaderValue) -> HeaderValue {
    let joined = parts.join(", ");
    HeaderValue::from_str(&joined).unwrap_or_else(|_| {
        tracing::warn!(
            header = header_name,
            value = ?joined,
            "CORS: configured list is not a valid header value — using fallback"
        );
        fallback
    })
}

/// Build the preflight (OPTIONS) response headers per the CORS spec.
///
/// Returns an empty `HeaderMap` when the origin is not in the allow-list or
/// when the origin is unsafe — the caller must still return 204, but without
/// CORS headers the browser will treat the preflight as rejected.
pub fn preflight_headers(cfg: &CorsConfig, request_origin: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    let Some(origin_value) = allowed_origin_value(cfg, request_origin) else {
        return h;
    };
    tracing::debug!(
        origin = request_origin,
        "CORS preflight: origin allowed — building preflight headers"
    );
    // Use `insert` (overwrites any previous value); there is at most one of
    // each CORS header per response.
    h.insert("access-control-allow-origin", origin_value);
    if cfg.allow_credentials {
        h.insert(
            "access-control-allow-credentials",
            HeaderValue::from_static("true"),
        );
    }
    if !cfg.allow_methods.is_empty() {
        h.insert(
            "access-control-allow-methods",
            joined_header_value(
                &cfg.allow_methods,
                "access-control-allow-methods",
                HeaderValue::from_static("GET"),
            ),
        );
    }
    if !cfg.allow_headers.is_empty() {
        h.insert(
            "access-control-allow-headers",
            joined_header_value(
                &cfg.allow_headers,
                "access-control-allow-headers",
                HeaderValue::from_static("Content-Type"),
            ),
        );
    }
    if cfg.max_age_secs > 0 {
        // `From<u64>` renders the decimal digits — infallible by type.
        h.insert(
            "access-control-max-age",
            HeaderValue::from(cfg.max_age_secs),
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
    let Some(origin_value) = allowed_origin_value(cfg, origin) else {
        return h;
    };
    tracing::debug!(
        origin,
        "CORS response: origin allowed — adding CORS headers"
    );
    h.insert("access-control-allow-origin", origin_value);
    if cfg.allow_credentials {
        h.insert(
            "access-control-allow-credentials",
            HeaderValue::from_static("true"),
        );
    }
    if !cfg.expose_headers.is_empty() {
        h.insert(
            "access-control-expose-headers",
            joined_header_value(
                &cfg.expose_headers,
                "access-control-expose-headers",
                HeaderValue::from_static(""),
            ),
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

    // ─── treat-as-absent fallbacks (never panic the request task) ───────────

    #[test]
    fn preflight_origin_with_control_char_treated_as_absent() {
        // ASCII + no CR/LF passes is_safe_origin, but NUL is not a legal
        // header byte — must degrade to empty headers, never panic.
        let cfg = cfg_with_origins(&["*"]);
        let h = preflight_headers(&cfg, "https://ev\u{0}il.com");
        assert!(
            h.is_empty(),
            "control-char origin must produce empty headers"
        );
    }

    #[test]
    fn response_headers_origin_with_control_char_treated_as_absent() {
        let cfg = cfg_with_origins(&["*"]);
        let h = response_headers(&cfg, Some("https://ev\u{1}il.com"));
        assert!(
            h.is_empty(),
            "control-char origin must produce empty headers"
        );
    }

    #[test]
    fn preflight_malformed_allow_methods_falls_back_to_get() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.allow_methods = vec!["GET\r\nX-Evil: 1".into()];
        let h = preflight_headers(&cfg, "https://example.com");
        assert_eq!(
            h.get("access-control-allow-methods")
                .and_then(|v| v.to_str().ok()),
            Some("GET"),
            "malformed configured methods must fall back, not panic"
        );
        assert!(h.get("x-evil").is_none(), "no header injection via config");
    }

    #[test]
    fn preflight_malformed_allow_headers_falls_back_to_content_type() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.allow_headers = vec!["X-Ok\nX-Evil: 1".into()];
        let h = preflight_headers(&cfg, "https://example.com");
        assert_eq!(
            h.get("access-control-allow-headers")
                .and_then(|v| v.to_str().ok()),
            Some("Content-Type")
        );
        assert!(h.get("x-evil").is_none(), "no header injection via config");
    }

    #[test]
    fn response_malformed_expose_headers_falls_back_to_empty() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.expose_headers = vec!["X-Ok\r\nX-Evil: 1".into()];
        let h = response_headers(&cfg, Some("https://example.com"));
        assert_eq!(
            h.get("access-control-expose-headers")
                .and_then(|v| v.to_str().ok()),
            Some("")
        );
        assert!(h.get("x-evil").is_none(), "no header injection via config");
    }

    #[test]
    fn preflight_max_age_renders_u64_decimal() {
        let mut cfg = cfg_with_origins(&["https://example.com"]);
        cfg.max_age_secs = u64::MAX;
        let h = preflight_headers(&cfg, "https://example.com");
        assert_eq!(
            h.get("access-control-max-age")
                .and_then(|v| v.to_str().ok()),
            Some("18446744073709551615")
        );
    }
}
