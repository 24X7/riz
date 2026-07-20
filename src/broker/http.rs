//! Outbound-HTTP backend for the broker's `http.fetch` verb.
//!
//! The guest never holds a credential or an absolute URL: it names a grant and
//! supplies a path RELATIVE to the resource's `base_url`. The daemon joins the
//! two, confines the result to paths under `base_url`, injects the auth header
//! host-side, and dials — under SSRF hardening.
//!
//! SSRF posture:
//! - **Post-DNS private-IP refusal, TOCTOU-closed.** A custom reqwest DNS
//!   resolver ([`PublicOnlyResolver`]) resolves the host and returns ONLY
//!   public addresses; if a host resolves solely to loopback/private/
//!   link-local space the connection fails. Because reqwest dials exactly the
//!   addresses the resolver returns, a DNS-rebinding race cannot slip an
//!   internal IP past the check.
//! - **Redirects disabled.** A 3xx is returned to the guest as-is; the daemon
//!   never follows a redirect into internal space.
//! - **Method allow-list per grant** and **origin pinning** (relative path
//!   under `base_url`) confine what a guest can reach.

use super::PgRows;
use crate::config::{HttpAuth, HttpResourceConfig};
use std::net::IpAddr;
use std::sync::Arc;

/// A `http`-type grant backend: an operator-pinned origin + host-held auth.
pub struct HttpBackend {
    client: reqwest::Client,
    base_url: reqwest::Url,
    /// Resolved auth header (name, value) injected on every request. `None`
    /// when the resource declares no `auth`.
    auth_header: Option<(String, String)>,
}

impl HttpBackend {
    /// Build from a resource config; resolves `auth.token_env` NOW so a missing
    /// secret is a daemon-startup error, not a first-request surprise.
    pub fn from_resource(res: &HttpResourceConfig) -> Result<Self, String> {
        let mut base_url = reqwest::Url::parse(&res.base_url)
            .map_err(|e| format!("invalid base_url '{}': {e}", res.base_url))?;
        match base_url.scheme() {
            "http" | "https" => {}
            other => return Err(format!("base_url scheme '{other}' must be http or https")),
        }
        // Normalize to a trailing slash so a relative guest path appends UNDER
        // the prefix (`/v1` + `charges` → `/v1/charges`), not replacing the
        // last segment as bare `Url::join` would (`/v1` + `charges` → `/charges`).
        if !base_url.path().ends_with('/') {
            let with_slash = format!("{}/", base_url.path());
            base_url.set_path(&with_slash);
        }
        let auth_header = match &res.auth {
            Some(auth) => Some(resolve_auth(auth)?),
            None => None,
        };
        let mut builder = reqwest::Client::builder()
            // Never follow a redirect — a 3xx could point into internal space.
            .redirect(reqwest::redirect::Policy::none());
        if !res.allow_private_ips {
            // SSRF gate: dial only the public addresses this resolver returns.
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder
            .build()
            .map_err(|e| format!("http client build failed: {e}"))?;
        Ok(Self {
            client,
            base_url,
            auth_header,
        })
    }

    /// Perform one brokered request. `method`/`path` come from the guest;
    /// `path` is joined under `base_url` and confined to it. Returns the
    /// response as a `{status, headers, body}` object wrapped in `PgRows`'
    /// single-row convention so the dispatcher's envelope layer is reused.
    pub async fn fetch(
        &self,
        method: &str,
        path: &str,
        allowed_methods: &[String],
        body: Option<&str>,
    ) -> Result<PgRows, String> {
        // Method allow-list (case-insensitive).
        let method_uc = method.to_ascii_uppercase();
        if !allowed_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&method_uc))
        {
            return Err(format!("method '{method}' is not permitted by this grant"));
        }
        let reqmethod = reqwest::Method::from_bytes(method_uc.as_bytes())
            .map_err(|_| format!("invalid HTTP method '{method}'"))?;

        // Origin pinning: join the guest path under base_url and require the
        // result to stay under base_url's origin + path prefix. A guest that
        // supplies an absolute URL or a `..` escape is confined back.
        let target = self.resolve_target(path)?;

        let mut builder = self.client.request(reqmethod, target);
        if let Some((name, value)) = &self.auth_header {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(b) = body {
            builder = builder.body(b.to_string());
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let status = resp.status().as_u16();
        let mut headers = serde_json::Map::new();
        for (k, v) in resp.headers() {
            if let Ok(s) = v.to_str() {
                headers.insert(k.as_str().to_string(), serde_json::Value::from(s));
            }
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading response body failed: {e}"))?;

        Ok(PgRows {
            rows: vec![serde_json::json!({
                "status": status,
                "headers": headers,
                "body": text,
            })],
        })
    }

    /// Join a guest-supplied relative path under `base_url` and confine it to
    /// the base origin + path prefix. Rejects absolute URLs and prefix escapes.
    fn resolve_target(&self, path: &str) -> Result<reqwest::Url, String> {
        // A scheme-bearing input is an absolute URL — the guest must not name
        // an origin; it supplies a path only.
        if path.contains("://") {
            return Err(
                "path must be relative to the resource base_url, not an absolute URL".into(),
            );
        }
        let joined = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|e| format!("could not resolve path '{path}': {e}"))?;
        // Confinement: the joined URL must share the base origin AND its path
        // must remain under the base path prefix (blocks `..` traversal).
        if joined.origin() != self.base_url.origin() {
            return Err("resolved URL escapes the resource origin".into());
        }
        if !joined.path().starts_with(self.base_url.path()) {
            return Err("resolved URL escapes the resource path prefix".into());
        }
        Ok(joined)
    }
}

/// Resolve an [`HttpAuth`] into the concrete (header-name, header-value) pair
/// the daemon injects. The token is read from the host env here and never
/// leaves the daemon.
fn resolve_auth(auth: &HttpAuth) -> Result<(String, String), String> {
    let token = std::env::var(&auth.token_env).map_err(|_| {
        format!(
            "auth token_env '{}' is not set in the host environment",
            auth.token_env
        )
    })?;
    match auth.kind.as_str() {
        "bearer" => Ok(("Authorization".to_string(), format!("Bearer {token}"))),
        "header" => {
            let header = auth
                .header
                .clone()
                .ok_or_else(|| "auth kind 'header' requires a `header` name".to_string())?;
            Ok((header, token))
        }
        other => Err(format!("auth kind '{other}' must be 'bearer' or 'header'")),
    }
}

/// True when `ip` is a globally-routable address we will dial. Refuses
/// loopback, private, link-local, unspecified, multicast, broadcast,
/// documentation, and IPv6 ULA/link-local — the SSRF surface.
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation())
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return false;
            }
            // Map IPv4-in-IPv6 back to the v4 checks.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(&IpAddr::V4(v4));
            }
            let first = v6.segments().first().copied().unwrap_or(0);
            // fc00::/7 (unique local) and fe80::/10 (link local).
            let is_ula = (first & 0xfe00) == 0xfc00;
            let is_link_local = (first & 0xffc0) == 0xfe80;
            !(is_ula || is_link_local)
        }
    }
}

/// A reqwest DNS resolver that returns only public addresses. If a host
/// resolves solely to private/internal space the connection fails, and because
/// reqwest dials exactly these addresses the check is not subject to a
/// DNS-rebinding TOCTOU.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public: Vec<std::net::SocketAddr> =
                addrs.filter(|a| is_public_ip(&a.ip())).collect();
            if public.is_empty() {
                return Err(
                    format!("host '{host}' resolves only to private/internal addresses").into(),
                );
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_classification() {
        assert!(is_public_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_public_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip(&"169.254.1.1".parse().unwrap())); // link-local
        assert!(!is_public_ip(&"::1".parse().unwrap()));
        assert!(!is_public_ip(&"fd00::1".parse().unwrap())); // ULA
        assert!(!is_public_ip(&"fe80::1".parse().unwrap())); // link-local
        assert!(is_public_ip(&"2606:4700:4700::1111".parse().unwrap()));
    }
}
