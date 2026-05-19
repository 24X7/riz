use std::collections::HashMap;
use crate::config::RouteConfig;

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
            params.insert(name.to_string(), seg.to_string());
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
}
