//! Phase 3 — Clerk JWT/JWKS validation, proven end-to-end with REAL cryptography.
//!
//! Mirrors tests/auth_workos.rs but for Clerk's token shape, and specifically
//! proves the optional-audience gap fix: Clerk's DEFAULT session token carries
//! NO `aud` claim, so the riz JWT authorizer must accept it when no audience is
//! configured (omitted/empty `audience` in riz.toml).
//!
//! Clerk session token shape:
//!   iss  https://<slug>.clerk.accounts.dev
//!   alg  RS256, header kid
//!   sub, sid, azp, exp, iat, nbf   (NO `aud` by default)

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;

use riz::auth::authorizer::{AuthError, Authorizer};
use riz::config::JwtAuthorizerConfig;
use riz::gateway::ApiGatewayV2httpRequest;

const KID: &str = "test-1";
const ISSUER: &str = "https://example-app.clerk.accounts.dev";

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct TestKey {
    encoding: EncodingKey,
    jwks: String,
}

impl TestKey {
    fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA key");
        let public = RsaPublicKey::from(&private);

        let n = b64url(&public.n().to_bytes_be());
        let e = b64url(&public.e().to_bytes_be());
        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "kid": KID,
                "alg": "RS256",
                "n": n,
                "e": e,
            }]
        })
        .to_string();

        let der = private.to_pkcs1_der().expect("pkcs1 der");
        let encoding = EncodingKey::from_rsa_der(der.as_bytes());

        Self { encoding, jwks }
    }

    fn sign(&self, claims: &serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_string());
        jsonwebtoken::encode(&header, claims, &self.encoding).expect("sign token")
    }
}

fn serve_jwks(jwks: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .ok();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = jwks.clone();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/.well-known/jwks.json")
}

/// Clerk config: issuer + JWKS, NO audience (empty) — the default-session shape.
fn clerk_config(jwks_uri: String) -> JwtAuthorizerConfig {
    JwtAuthorizerConfig {
        r#type: "jwt".into(),
        issuer: ISSUER.into(),
        audience: String::new(), // Clerk default token has no `aud`
        jwks_uri,
    }
}

fn event_with_token(token: &str) -> ApiGatewayV2httpRequest {
    let mut event = riz::test_helpers::make_event("GET", "/api");
    event.headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    event
}

/// A valid Clerk DEFAULT session token: issuer, sub/sid/azp, nbf/iat/exp, NO aud.
fn valid_clerk_claims() -> serde_json::Value {
    let now = now();
    json!({
        "iss": ISSUER,
        "sub": "user_2ClerkUserId",
        "sid": "sess_2ClerkSessionId",
        "azp": "https://app.example.com",
        "iat": now,
        "nbf": now - 5,
        "exp": now + 3600,
        // deliberately NO "aud"
    })
}

// ── valid-token proof (this fn name is the registry proof) ────────────────────

/// The registry proof for `auth-clerk`: a real Clerk DEFAULT session token —
/// which carries NO `aud` claim — is ACCEPTED when riz is configured without an
/// audience. This proves the optional-audience gap fix.
#[tokio::test]
async fn clerk_default_token_without_aud_is_accepted() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(clerk_config(jwks_uri))
        .await
        .expect("construct JwtAuthorizer from served JWKS");

    let token = key.sign(&valid_clerk_claims());
    let out = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect("Clerk default token (no aud) must be accepted when audience is unset");

    assert_eq!(out.principal_id, "user_2ClerkUserId");
    assert_eq!(
        out.context.get("iss").and_then(|v| v.as_str()),
        Some(ISSUER)
    );
    assert_eq!(
        out.context.get("sid").and_then(|v| v.as_str()),
        Some("sess_2ClerkSessionId"),
        "Clerk sid claim must be surfaced into context"
    );
    assert_eq!(
        out.context.get("azp").and_then(|v| v.as_str()),
        Some("https://app.example.com")
    );
    assert!(
        out.context.get("aud").is_none(),
        "Clerk default token has no aud; context must not invent one"
    );
}

// ── negative cases ────────────────────────────────────────────────────────────

#[tokio::test]
async fn clerk_wrong_issuer_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(clerk_config(jwks_uri))
        .await
        .unwrap();

    let mut claims = valid_clerk_claims();
    claims["iss"] = json!("https://attacker.clerk.accounts.dev");
    let token = key.sign(&claims);
    let err = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect_err("wrong-issuer Clerk token must be rejected even with no audience");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

#[tokio::test]
async fn clerk_expired_token_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(clerk_config(jwks_uri))
        .await
        .unwrap();

    let now = now();
    let claims = json!({
        "iss": ISSUER,
        "sub": "user_2ClerkUserId",
        "sid": "sess_2ClerkSessionId",
        "iat": now - 7200,
        "exp": now - 3600,
    });
    let token = key.sign(&claims);
    let err = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect_err("expired Clerk token must be rejected");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

#[tokio::test]
async fn clerk_tampered_signature_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(clerk_config(jwks_uri))
        .await
        .unwrap();

    let token = key.sign(&valid_clerk_claims());
    let mut parts: Vec<String> = token.split('.').map(|s| s.to_string()).collect();
    let sig = &mut parts[2];
    let last = sig.pop().unwrap();
    sig.push(if last == 'A' { 'B' } else { 'A' });
    let tampered = parts.join(".");

    let err = authorizer
        .authorize(&event_with_token(&tampered))
        .await
        .expect_err("tampered-signature Clerk token must be rejected");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}
