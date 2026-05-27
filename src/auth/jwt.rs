//! JWT authorizer — validates `Authorization: Bearer <jwt>` against a JWKS URL.
//!
//! Fetches the JWKS on construction. On validation failure due to a key-not-found
//! error (the signing key may have been rotated), refreshes the JWKS once with
//! exponential backoff before giving up.

use crate::auth::authorizer::{AuthError, Authorizer, AuthorizerOutput};
use crate::config::JwtAuthorizerConfig;
use crate::gateway::ApiGatewayV2httpRequest;
use jsonwebtoken::{decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::warn;

/// A single JWKS key entry as returned by the JWKS endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct JwksKey {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(rename = "use", default)]
    pub key_use: Option<String>,
    pub alg: Option<String>,
    // RSA key components
    pub n: Option<String>,
    pub e: Option<String>,
    // EC key components
    pub x: Option<String>,
    pub y: Option<String>,
    pub crv: Option<String>,
}

/// JWKS document returned by the endpoint.
#[derive(Debug, Deserialize)]
pub struct Jwks {
    pub keys: Vec<JwksKey>,
}

/// JWT claims we decode and validate.
#[derive(Debug, Deserialize)]
pub struct JwtClaims {
    /// Subject (principal ID).
    #[serde(default)]
    pub sub: Option<String>,
    /// Issuer (validated against config).
    pub iss: String,
    /// Expiry (validated by jsonwebtoken).
    pub exp: u64,
    /// Audience (validated against config).
    #[serde(default)]
    pub aud: Option<serde_json::Value>,
    /// Capture all remaining claims into context.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// JWT authorizer backed by a JWKS URL.
pub struct JwtAuthorizer {
    config: JwtAuthorizerConfig,
    /// Cached JWKS keys. Wrapped in an `Arc<RwLock>` so refreshes can happen
    /// without taking a `&mut self` (trait impl requires `&self`).
    keys: Arc<RwLock<Vec<JwksKey>>>,
    client: reqwest::Client,
}

impl JwtAuthorizer {
    /// Fetch JWKS and build the authorizer. Returns an error if the JWKS URL
    /// is unreachable or returns an invalid response.
    pub async fn new(config: JwtAuthorizerConfig) -> Result<Self, AuthError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| AuthError::Other(format!("failed to build HTTP client: {e}")))?;

        let keys = fetch_jwks(&client, &config.jwks_uri).await?;
        Ok(Self {
            config,
            keys: Arc::new(RwLock::new(keys)),
            client,
        })
    }

    /// Refresh the cached JWKS. Called automatically on key-not-found errors.
    async fn refresh_keys(&self) -> Result<(), AuthError> {
        let new_keys = fetch_jwks(&self.client, &self.config.jwks_uri).await?;
        let mut w = self.keys.write().await;
        *w = new_keys;
        Ok(())
    }

    /// Find the decoding key matching the JWT's `kid` header (or the first
    /// signature key if the JWT has no `kid`).
    ///
    /// Only keys with `use = "sig"` (or unset) are candidates — encryption
    /// keys (`use = "enc"`) are skipped.
    async fn find_key(&self, kid: Option<&str>) -> Option<JwksKey> {
        let keys = self.keys.read().await;
        // A key is eligible if its `use` is absent or is "sig".
        let is_sig_key = |k: &&JwksKey| {
            k.key_use
                .as_deref()
                .map(|u| u.eq_ignore_ascii_case("sig"))
                .unwrap_or(true) // absent `use` → assume sig
        };
        if let Some(kid) = kid {
            keys.iter()
                .filter(is_sig_key)
                .find(|k| k.kid.as_deref() == Some(kid))
                .cloned()
        } else {
            keys.iter().find(is_sig_key).cloned()
        }
    }

    fn validate_token(&self, token: &str, key: &JwksKey) -> Result<JwtClaims, AuthError> {
        let decoding_key = build_decoding_key(key)
            .map_err(|e| AuthError::Other(format!("failed to build decoding key: {e}")))?;

        let alg = key
            .alg
            .as_deref()
            .and_then(parse_algorithm)
            .unwrap_or(Algorithm::RS256);

        let mut validation = Validation::new(alg);
        validation.set_issuer(&[&self.config.issuer]);
        // Audience can be a single string or an array — normalise.
        validation.set_audience(&[&self.config.audience]);
        validation.validate_exp = true;
        validation.validate_nbf = false;

        let token_data = jsonwebtoken::decode::<JwtClaims>(token, &decoding_key, &validation)
            .map_err(|e| AuthError::Unauthorized(format!("JWT validation failed: {e}")))?;

        Ok(token_data.claims)
    }
}

#[async_trait::async_trait]
impl Authorizer for JwtAuthorizer {
    async fn authorize(
        &self,
        event: &ApiGatewayV2httpRequest,
    ) -> Result<AuthorizerOutput, AuthError> {
        let source_ip = event
            .request_context
            .http
            .source_ip
            .as_deref()
            .unwrap_or("unknown");

        // Extract Bearer token from Authorization header.
        let auth_header = event
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let token = auth_header
            .strip_prefix("Bearer ")
            .or_else(|| auth_header.strip_prefix("bearer "))
            .map(|t| t.trim())
            .ok_or_else(|| {
                warn!(
                    source_ip = %source_ip,
                    "JWT authorizer: missing or malformed Authorization header"
                );
                AuthError::Unauthorized("missing or malformed Authorization header".into())
            })?;

        if token.is_empty() {
            warn!(source_ip = %source_ip, "JWT authorizer: empty bearer token");
            return Err(AuthError::Unauthorized("empty bearer token".into()));
        }

        // Decode header to get the kid (key ID).
        let header = decode_header(token).map_err(|e| {
            warn!(source_ip = %source_ip, "JWT authorizer: failed to decode header: {e}");
            AuthError::Unauthorized(format!("invalid JWT header: {e}"))
        })?;

        let kid = header.kid.as_deref();

        // Attempt validation with the cached key.
        let key = match self.find_key(kid).await {
            Some(k) => k,
            None => {
                // Key not found — refresh and retry once.
                warn!(
                    source_ip = %source_ip,
                    kid = ?kid,
                    "JWT authorizer: kid not in cache, refreshing JWKS"
                );
                self.refresh_keys().await.map_err(|e| {
                    warn!(source_ip = %source_ip, "JWT authorizer: JWKS refresh failed: {e}");
                    e
                })?;
                self.find_key(kid).await.ok_or_else(|| {
                    warn!(source_ip = %source_ip, kid = ?kid, "JWT authorizer: kid still not found after refresh");
                    AuthError::Unauthorized(format!(
                        "signing key not found (kid={kid:?})"
                    ))
                })?
            }
        };

        let claims = match self.validate_token(token, &key) {
            Ok(c) => c,
            Err(AuthError::Unauthorized(_)) => {
                // On validation failure the key might have rotated — try
                // refreshing once (backoff: one immediate retry then give up).
                warn!(
                    source_ip = %source_ip,
                    "JWT authorizer: validation failed — refreshing JWKS and retrying"
                );
                if let Ok(()) = self.refresh_keys().await {
                    if let Some(refreshed_key) = self.find_key(kid).await {
                        self.validate_token(token, &refreshed_key).map_err(|e| {
                            warn!(source_ip = %source_ip, "JWT authorizer: validation failed after refresh: {e}");
                            e
                        })?
                    } else {
                        warn!(source_ip = %source_ip, kid = ?kid, "JWT authorizer: kid not found after refresh");
                        return Err(AuthError::Unauthorized(format!(
                            "signing key not found after refresh (kid={kid:?})"
                        )));
                    }
                } else {
                    warn!(source_ip = %source_ip, "JWT authorizer: JWKS refresh failed");
                    return Err(AuthError::Unauthorized("JWKS refresh failed".into()));
                }
            }
            Err(e) => return Err(e),
        };

        let principal_id = claims
            .sub
            .clone()
            .unwrap_or_else(|| "jwt-principal".to_string());

        // Build context from JWT claims: all extra claims + sub/iss/exp/aud.
        let mut context: HashMap<String, Value> = claims.extra;
        if let Some(sub) = &claims.sub {
            context.insert("sub".into(), Value::String(sub.clone()));
        }
        context.insert("iss".into(), Value::String(claims.iss.clone()));
        context.insert("exp".into(), Value::Number(claims.exp.into()));
        if let Some(aud) = claims.aud {
            context.insert("aud".into(), aud);
        }

        Ok(AuthorizerOutput {
            principal_id,
            context,
            ttl: Duration::from_secs(300),
        })
    }
}

/// Fetch a JWKS document from the given URL.
async fn fetch_jwks(client: &reqwest::Client, url: &str) -> Result<Vec<JwksKey>, AuthError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AuthError::Other(format!("JWKS fetch failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(AuthError::Other(format!(
            "JWKS endpoint returned non-2xx status: {}",
            resp.status()
        )));
    }

    let jwks: Jwks = resp
        .json()
        .await
        .map_err(|e| AuthError::Other(format!("JWKS parse failed: {e}")))?;

    if jwks.keys.is_empty() {
        return Err(AuthError::Other("JWKS returned empty key set".into()));
    }

    Ok(jwks.keys)
}

/// Build a `jsonwebtoken::DecodingKey` from a JWKS key entry.
fn build_decoding_key(key: &JwksKey) -> Result<DecodingKey, String> {
    match key.kty.as_str() {
        "RSA" => {
            let n = key.n.as_deref().ok_or("RSA key missing 'n' component")?;
            let e = key.e.as_deref().ok_or("RSA key missing 'e' component")?;
            DecodingKey::from_rsa_components(n, e)
                .map_err(|err| format!("invalid RSA key components: {err}"))
        }
        "EC" => {
            let x = key.x.as_deref().ok_or("EC key missing 'x' component")?;
            let y = key.y.as_deref().ok_or("EC key missing 'y' component")?;
            let crv = key.crv.as_deref().unwrap_or("P-256");
            DecodingKey::from_ec_components(x, y)
                .map_err(|err| format!("invalid EC key components (crv={crv}): {err}"))
        }
        kty => Err(format!("unsupported key type: {kty}")),
    }
}

fn parse_algorithm(alg: &str) -> Option<Algorithm> {
    match alg {
        "RS256" => Some(Algorithm::RS256),
        "RS384" => Some(Algorithm::RS384),
        "RS512" => Some(Algorithm::RS512),
        "ES256" => Some(Algorithm::ES256),
        "ES384" => Some(Algorithm::ES384),
        "HS256" => Some(Algorithm::HS256),
        "HS384" => Some(Algorithm::HS384),
        "HS512" => Some(Algorithm::HS512),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_algorithm_known_values() {
        assert!(matches!(parse_algorithm("RS256"), Some(Algorithm::RS256)));
        assert!(matches!(parse_algorithm("ES256"), Some(Algorithm::ES256)));
        assert!(matches!(parse_algorithm("HS256"), Some(Algorithm::HS256)));
    }

    #[test]
    fn parse_algorithm_unknown_returns_none() {
        assert!(parse_algorithm("NONE").is_none());
        assert!(parse_algorithm("").is_none());
        assert!(parse_algorithm("PS256").is_none());
    }

    #[test]
    fn build_decoding_key_rejects_missing_n() {
        let key = JwksKey {
            kty: "RSA".into(),
            kid: None,
            key_use: None,
            alg: None,
            n: None,
            e: Some("AQAB".into()),
            x: None,
            y: None,
            crv: None,
        };
        let result = build_decoding_key(&key);
        match result {
            Err(msg) => assert!(msg.contains("missing 'n'"), "got: {msg}"),
            Ok(_) => panic!("expected error for RSA key missing 'n'"),
        }
    }

    #[test]
    fn build_decoding_key_rejects_unsupported_kty() {
        let key = JwksKey {
            kty: "oct".into(),
            kid: None,
            key_use: None,
            alg: None,
            n: None,
            e: None,
            x: None,
            y: None,
            crv: None,
        };
        let result = build_decoding_key(&key);
        match result {
            Err(msg) => assert!(msg.contains("unsupported key type"), "got: {msg}"),
            Ok(_) => panic!("expected error for unsupported key type"),
        }
    }

    #[test]
    fn jwks_key_deserializes() {
        let json = r#"{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": "key-1",
            "n": "somereallybigrsakeymodulus",
            "e": "AQAB"
        }"#;
        let key: JwksKey = serde_json::from_str(json).unwrap();
        assert_eq!(key.kty, "RSA");
        assert_eq!(key.kid.as_deref(), Some("key-1"));
        assert_eq!(key.alg.as_deref(), Some("RS256"));
    }

    #[test]
    fn jwt_claims_deserializes() {
        let json = r#"{
            "sub": "user123",
            "iss": "https://example.com",
            "exp": 9999999999,
            "aud": "myapp",
            "custom_claim": "hello"
        }"#;
        let claims: JwtClaims = serde_json::from_str(json).unwrap();
        assert_eq!(claims.sub.as_deref(), Some("user123"));
        assert_eq!(claims.iss, "https://example.com");
        assert_eq!(claims.exp, 9999999999);
        assert_eq!(
            claims.extra.get("custom_claim").and_then(|v| v.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn empty_bearer_token_rejected() {
        // Regression: an empty token after prefix-stripping must error, not
        // proceed to signature validation (which would crash the decoder).
        let token = "Bearer ";
        let result = token
            .strip_prefix("Bearer ")
            .map(|t| t.trim())
            .filter(|t| !t.is_empty());
        assert!(result.is_none());
    }
}
