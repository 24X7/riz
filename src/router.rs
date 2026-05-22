use std::collections::HashMap;
use crate::config::RouteConfig;

/// Decodes a percent-encoded string (e.g. "foo%2Fbar" → "foo/bar").
/// Matches AWS API Gateway behavior of decoding path parameters.
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut bytes = s.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h = bytes.next();
            let l = bytes.next();
            if let (Some(h), Some(l)) = (h, l) {
                if let (Some(hi), Some(lo)) = (hex_val(h), hex_val(l)) {
                    result.push(char::from(hi << 4 | lo));
                    continue;
                }
                // Invalid escape sequence, pass through
                result.push('%');
                result.push(char::from(h));
                result.push(char::from(l));
            } else {
                // Incomplete escape sequence
                result.push('%');
            }
        } else {
            result.push(char::from(b));
        }
    }
    result
}

/// Helper to convert a hex digit (0-9, a-f, A-F) to its numeric value.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub struct Router {
    routes: Vec<RouteConfig>,
}

pub struct RouteMatch<'a> {
    pub route: &'a RouteConfig,
    pub path_params: HashMap<String, String>,
}

impl Router {
    pub fn new(routes: Vec<RouteConfig>) -> Self {
        Self { routes }
    }

    /// Returns "METHOD /path/pattern" — the stable key used throughout the system.
    pub fn route_key(method: &str, pattern: &str) -> String {
        format!("{} {}", method.to_uppercase(), pattern)
    }

    pub fn match_route<'a>(&'a self, method: &str, path: &str) -> Option<RouteMatch<'a>> {
        let method_upper = method.to_uppercase();
        for route in &self.routes {
            if route.method.to_uppercase() != method_upper {
                continue;
            }
            if let Some(params) = match_pattern(&route.path, path) {
                return Some(RouteMatch { route, path_params: params });
            }
        }
        None
    }
}

/// Matches a route pattern (e.g. "/accounts/:id") against a concrete path.
fn match_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_parts: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path_parts: Vec<&str> = path.trim_matches('/').split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (pat, seg) in pattern_parts.iter().zip(path_parts.iter()) {
        if let Some(name) = pat.strip_prefix(':') {
            // Percent-decode the path segment to match AWS API Gateway behavior
            params.insert(name.to_string(), percent_decode(seg));
        } else if pat != seg {
            return None;
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::config::RuntimeKind;

    fn make_route(method: &str, path: &str) -> RouteConfig {
        RouteConfig {
            path: path.into(),
            method: method.into(),
            runtime: RuntimeKind::Bun,
            handler: PathBuf::from("./handler.ts"),
            timeout_ms: 5000,
            cache_ttl_secs: None,
            concurrency: 1,
        }
    }

    #[test]
    fn matches_exact_path() {
        let router = Router::new(vec![make_route("GET", "/ping")]);
        assert!(router.match_route("GET", "/ping").is_some());
        assert!(router.match_route("GET", "/pong").is_none());
    }

    #[test]
    fn matches_path_param() {
        let router = Router::new(vec![make_route("GET", "/accounts/:id")]);
        let m = router.match_route("GET", "/accounts/42").unwrap();
        assert_eq!(m.path_params["id"], "42");
    }

    #[test]
    fn method_mismatch_returns_none() {
        let router = Router::new(vec![make_route("GET", "/ping")]);
        assert!(router.match_route("POST", "/ping").is_none());
    }

    #[test]
    fn route_key_format() {
        assert_eq!(Router::route_key("get", "/accounts/:id"), "GET /accounts/:id");
    }

    #[test]
    fn no_match_on_different_segment_count() {
        let router = Router::new(vec![make_route("GET", "/a/b")]);
        assert!(router.match_route("GET", "/a").is_none());
        assert!(router.match_route("GET", "/a/b/c").is_none());
    }

    #[test]
    fn case_insensitive_method() {
        let router = Router::new(vec![make_route("GET", "/ping")]);
        assert!(router.match_route("get", "/ping").is_some());
        assert!(router.match_route("Get", "/ping").is_some());
    }

    #[test]
    fn percent_decode_handles_encoded_slash() {
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
    }

    #[test]
    fn percent_decode_handles_space() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
    }

    #[test]
    fn percent_decode_passthrough_unencoded() {
        assert_eq!(percent_decode("normal"), "normal");
    }

    #[test]
    fn percent_decode_mixed_encoded_and_unencoded() {
        assert_eq!(percent_decode("foo%2Fbar/baz"), "foo/bar/baz");
    }

    #[test]
    fn matches_path_param_with_percent_encoding() {
        let router = Router::new(vec![make_route("GET", "/accounts/:id")]);
        let m = router.match_route("GET", "/accounts/foo%2Fbar").unwrap();
        // Path parameter should be decoded
        assert_eq!(m.path_params["id"], "foo/bar");
    }
}
