//! Phase 3 — WorkOS (AuthKit) JWT/JWKS validation, proven end-to-end with REAL
//! cryptography.
//!
//! These are not config-parse smoke tests: each test generates an ephemeral RSA
//! keypair, serves a JWKS document built from its public components on a local
//! `TcpListener`, points a real `JwtAuthorizer` at that JWKS, mints a
//! WorkOS-shaped RS256 token signed with the private key, and asserts the
//! authorizer accepts the valid token and rejects every tampered/expired/
//! wrong-issuer/wrong-audience variant.
//!
//! WorkOS AuthKit token shape:
//!   iss  https://<app>.authkit.app   (user-management issuer)
//!   alg  RS256, header kid
//!   aud  the configured WorkOS client id (standard `aud` present + enforced)
//!   sub, sid, org_id, role, exp, iat

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
const ISSUER: &str = "https://my-app.authkit.app";
const AUDIENCE: &str = "client_01HXAMPLEWORKOSID";

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// An ephemeral RSA keypair plus everything the tests need: the JWKS document
/// (served to the authorizer) and a `jsonwebtoken` EncodingKey (used to sign).
struct TestKey {
    encoding: EncodingKey,
    jwks: String,
}

impl TestKey {
    fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA key");
        let public = RsaPublicKey::from(&private);

        // JWKS modulus/exponent are base64url(big-endian) of the components.
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

    /// Sign a token with header kid = KID (override alg/kid for negative tests).
    fn sign(&self, claims: &serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_string());
        jsonwebtoken::encode(&header, claims, &self.encoding).expect("sign token")
    }
}

/// Serve the JWKS document at `/.well-known/jwks.json` on a local listener.
/// Returns the base URL. The listener thread answers every connection with the
/// JWKS so construction + any refresh both succeed.
fn serve_jwks(jwks: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
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

fn workos_config(jwks_uri: String) -> JwtAuthorizerConfig {
    JwtAuthorizerConfig {
        r#type: "jwt".into(),
        issuer: ISSUER.into(),
        audience: AUDIENCE.into(),
        jwks_uri,
    }
}

/// Build an event carrying `Authorization: Bearer <token>`.
fn event_with_token(token: &str) -> ApiGatewayV2httpRequest {
    let mut event = riz::test_helpers::make_event("GET", "/api");
    event.headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    event
}

/// A valid WorkOS AuthKit token: correct issuer, audience, future exp.
fn valid_workos_claims() -> serde_json::Value {
    let now = now();
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": "user_01HWORKOSUSER",
        "sid": "session_01HWORKOSSESSION",
        "org_id": "org_01HWORKOSORG",
        "role": "admin",
        "iat": now,
        "exp": now + 3600,
    })
}

// ── valid-token proof (this fn name is the registry proof) ────────────────────

/// The registry proof for `auth-workos`: a real WorkOS-shaped RS256 token,
/// signed with an ephemeral key and validated against the served JWKS, is
/// ACCEPTED and its claims are surfaced into the authorizer context.
#[tokio::test]
async fn workos_valid_token_is_accepted_and_claims_present() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .expect("construct JwtAuthorizer from served JWKS");

    let token = key.sign(&valid_workos_claims());
    let out = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect("valid WorkOS token must be accepted");

    assert_eq!(out.principal_id, "user_01HWORKOSUSER");
    assert_eq!(
        out.context.get("iss").and_then(|v| v.as_str()),
        Some(ISSUER)
    );
    assert_eq!(
        out.context.get("org_id").and_then(|v| v.as_str()),
        Some("org_01HWORKOSORG"),
        "WorkOS org_id claim must be surfaced into context"
    );
    assert_eq!(
        out.context.get("sid").and_then(|v| v.as_str()),
        Some("session_01HWORKOSSESSION")
    );
    assert_eq!(
        out.context.get("role").and_then(|v| v.as_str()),
        Some("admin")
    );
}

// ── negative cases ────────────────────────────────────────────────────────────

#[tokio::test]
async fn workos_expired_token_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .unwrap();

    let now = now();
    let claims = json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": "user_01HWORKOSUSER",
        "iat": now - 7200,
        "exp": now - 3600, // expired an hour ago
    });
    let token = key.sign(&claims);
    let err = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect_err("expired token must be rejected");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

#[tokio::test]
async fn workos_wrong_issuer_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .unwrap();

    let mut claims = valid_workos_claims();
    claims["iss"] = json!("https://evil.authkit.app");
    let token = key.sign(&claims);
    let err = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect_err("wrong-issuer token must be rejected");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

#[tokio::test]
async fn workos_wrong_audience_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .unwrap();

    let mut claims = valid_workos_claims();
    claims["aud"] = json!("client_SOMEONE_ELSE");
    let token = key.sign(&claims);
    let err = authorizer
        .authorize(&event_with_token(&token))
        .await
        .expect_err("wrong-audience token must be rejected (WorkOS enforces aud)");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

#[tokio::test]
async fn workos_tampered_signature_is_rejected() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .unwrap();

    let token = key.sign(&valid_workos_claims());
    // Flip a character in the signature segment (the third dot-section).
    let mut parts: Vec<String> = token.split('.').map(|s| s.to_string()).collect();
    let sig = &mut parts[2];
    let last = sig.pop().unwrap();
    sig.push(if last == 'A' { 'B' } else { 'A' });
    let tampered = parts.join(".");

    let err = authorizer
        .authorize(&event_with_token(&tampered))
        .await
        .expect_err("tampered-signature token must be rejected");
    assert!(matches!(err, AuthError::Unauthorized(_)), "got: {err:?}");
}

// ── P1.1: the AuthCache JWKS-authorizer cache ──────────────────────────────
//
// Repeated decision-cache-missed requests (e.g. a stream of distinct invalid
// Bearer tokens) must reuse one JWKS fetch, not fire one at the IdP per
// request. `AuthCache::jwt_authorizer` caches the constructed authorizer keyed
// by `jwks_uri` with a refresh cooldown, single-flighting construction.

/// Like `serve_jwks`, but counts how many times the JWKS document is fetched
/// (one accepted connection = one fetch, since we answer `Connection: close`).
fn serve_jwks_counting(jwks: String) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let hits_thread = hits.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            hits_thread.fetch_add(1, Ordering::SeqCst);
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
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
    (format!("http://{addr}/.well-known/jwks.json"), hits)
}

#[tokio::test]
async fn jwks_authorizer_cache_fetches_once_within_cooldown() {
    use std::sync::atomic::Ordering;
    let key = TestKey::generate();
    let (jwks_uri, hits) = serve_jwks_counting(key.jwks);
    let cfg = workos_config(jwks_uri);
    let cache = riz::auth::authorizer::AuthCache::new();

    // Five cache-missed authorizer builds within the cooldown window.
    for _ in 0..5 {
        cache.jwt_authorizer(&cfg).await.expect("authorizer builds");
    }

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "JWKS must be fetched exactly once across repeated cache-missed builds"
    );
}

#[tokio::test]
async fn jwks_authorizer_cache_fails_closed_when_unreachable() {
    // Port 1 has nothing listening — construction (JWKS fetch) must fail.
    let cfg = workos_config("http://127.0.0.1:1/.well-known/jwks.json".to_string());
    let cache = riz::auth::authorizer::AuthCache::new();

    assert!(
        cache.jwt_authorizer(&cfg).await.is_err(),
        "an unreachable JWKS must fail closed"
    );
    // Nothing is cached on error, so a second attempt also fails rather than
    // serving a stale authorizer.
    assert!(
        cache.jwt_authorizer(&cfg).await.is_err(),
        "a failed build must not be cached"
    );
}

// ── P4.1: parser assurance — the authorizer never panics on hostile input ──
//
// The Bearer token is fully attacker-controlled. Whatever bytes arrive, the
// authorizer must reject them with an error — never panic (which would crash
// the request task) and never accept an unsigned/garbage token. This is a
// fuzz-lite corpus test: a batch of random and structurally-adversarial tokens
// plus fixed edge cases, all fed through the real JWKS-backed authorizer.

fn random_token(rng: &mut impl rand::Rng) -> String {
    let len = rng.gen_range(0..512);
    // Constrained to visible ASCII (0x21..=0x7E): the token reaches the
    // authorizer only after passing the HTTP header-value grammar, so bytes
    // that can't be a header value are rejected by the transport, never by us.
    // Within that space we cover both "not a JWT at all" and (second branch)
    // "JWT-shaped but bogus".
    if rng.gen_bool(0.5) {
        (0..len)
            .map(|_| char::from(rng.gen_range(0x21u8..=0x7E)))
            .collect()
    } else {
        let seg = |rng: &mut dyn rand::RngCore| -> String {
            let n = (rng.next_u32() % 200) as usize;
            (0..n)
                .map(|_| {
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
                        [(rng.next_u32() % 64) as usize] as char
                })
                .collect()
        };
        format!("{}.{}.{}", seg(rng), seg(rng), seg(rng))
    }
}

#[tokio::test]
async fn authorizer_rejects_arbitrary_tokens_without_panic() {
    let key = TestKey::generate();
    let jwks_uri = serve_jwks(key.jwks.clone());
    let authorizer = riz::auth::jwt::JwtAuthorizer::new(workos_config(jwks_uri))
        .await
        .expect("construct authorizer");

    // Fixed adversarial edge cases (all header-value-legal — the transport
    // rejects anything else before the authorizer runs).
    let oversized = "x".repeat(200_000);
    let fixed = [
        "",
        ".",
        "..",
        "...",
        "a.b.c",
        "Bearer",
        "~!@#$%^&*()",
        "eyJhbGciOiJub25lIn0..", // alg=none, empty sig — the classic bypass
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhZG1pbiJ9.", // alg=none with a claims body
        oversized.as_str(),
    ];
    for token in fixed {
        let res = authorizer.authorize(&event_with_token(token)).await;
        assert!(
            res.is_err(),
            "adversarial token {token:?} must be rejected, not accepted"
        );
    }

    // Randomized corpus — reaching the assert at all proves no panic.
    let mut rng = rand::thread_rng();
    for _ in 0..500 {
        let token = random_token(&mut rng);
        let res = authorizer.authorize(&event_with_token(&token)).await;
        assert!(
            res.is_err(),
            "random token must never be accepted: {token:?}"
        );
    }
}
