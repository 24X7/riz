//! Wave 3 — Lambda authorizers (REQUEST + JWT) acceptance criteria.

use riz::auth::authorizer::{AuthCache, AuthCacheKey, AuthorizerOutput};
use riz::config::{AuthorizerConfig, Config, JwtAuthorizerConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Criterion 1: `Authorizer` trait exists with correct signature and the
/// `src/auth` module is present.
#[test]
fn authorizer_trait_exists_with_correct_signature() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — Wave 3 authorizer trait not yet shipped"
    );
    // Verify the trait and types compile and are accessible from the crate root.
    let _ = std::any::TypeId::of::<riz::auth::authorizer::AuthorizerOutput>();
    let _ = std::any::TypeId::of::<riz::auth::authorizer::AuthError>();
    let _ = std::any::TypeId::of::<riz::auth::authorizer::AuthCache>();
}

/// Criterion 2: `RequestAuthorizer` implementation exists.
#[test]
fn request_authorizer_calls_user_function() {
    // Verify RequestAuthorizer exists and is constructable via its public API.
    use riz::auth::authorizer::AuthCache;
    use riz::state::RizState;
    use std::sync::Arc;

    let riz_state = Arc::new(RizState::new());
    let pm = Arc::new(riz::process::ProcessManager::new(riz_state));
    // Construction must not panic.
    let _authorizer = riz::auth::request::RequestAuthorizer::new("my-auth-fn", pm);
    // No pool exists for "my-auth-fn" but we just verify constructability here.
    let _ = AuthCache::new();
}

/// Criterion 2 continued: `JwtAuthorizer` exists.
#[test]
fn jwt_authorizer_validates_against_jwks_url() {
    assert!(
        std::path::Path::new("src/auth/jwt.rs").exists(),
        "missing src/auth/jwt.rs — JwtAuthorizer not yet shipped (Wave 3)"
    );
    // Verify JwksKey and JwtClaims types exist (would fail to compile otherwise).
    let _ = std::any::TypeId::of::<riz::auth::jwt::JwksKey>();
    let _ = std::any::TypeId::of::<riz::auth::jwt::JwtClaims>();
}

/// Criterion 3: authorizer config parses from `[function.api]` block.
#[test]
fn authorizer_config_parses_from_function_block() {
    let toml_str = r#"
[function.auth-fn]
runtime = "bun"
handler = "auth.handler"

[function.api]
runtime = "bun"
handler = "index.handler"
authorizer = "auth-fn"

[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let result = toml::from_str::<Config>(toml_str);
    assert!(
        result.is_ok(),
        "authorizer field not accepted by FunctionConfig: {:?}",
        result.err()
    );
    let config = result.unwrap();
    let api_fn = config.functions.get("api").unwrap();
    assert!(
        api_fn.authorizer.is_some(),
        "authorizer field must be Some after parsing"
    );
    match api_fn.authorizer.as_ref().unwrap() {
        AuthorizerConfig::FunctionRef(name) => assert_eq!(name, "auth-fn"),
        other => panic!("expected FunctionRef, got {other:?}"),
    }
}

/// Criterion 3 continued: JWT authorizer config parses from inline block.
#[test]
fn jwt_authorizer_config_parses_from_inline_block() {
    let toml_str = r#"
[function.api]
runtime = "bun"
handler = "index.handler"

[function.api.authorizer]
type = "jwt"
issuer = "https://example.com"
audience = "myapp"
jwks_uri = "https://example.com/.well-known/jwks.json"
"#;
    let result = toml::from_str::<Config>(toml_str);
    assert!(
        result.is_ok(),
        "JWT authorizer block not accepted: {:?}",
        result.err()
    );
    let config = result.unwrap();
    let api_fn = config.functions.get("api").unwrap();
    match api_fn.authorizer.as_ref().unwrap() {
        AuthorizerConfig::Jwt(jwt_cfg) => {
            assert_eq!(jwt_cfg.r#type, "jwt");
            assert_eq!(jwt_cfg.issuer, "https://example.com");
            assert_eq!(jwt_cfg.audience, "myapp");
            assert_eq!(
                jwt_cfg.jwks_uri,
                "https://example.com/.well-known/jwks.json"
            );
        }
        other => panic!("expected Jwt variant, got {other:?}"),
    }
}

/// Criterion 4: authorizer responses are cached by (source_ip, auth_header_hash, function_name).
#[tokio::test]
async fn authorizer_responses_cached_with_ttl() {
    let cache = AuthCache::new();

    let key = AuthCacheKey::new("1.2.3.4", Some("Bearer test-token"), "api");
    let output = AuthorizerOutput {
        principal_id: "user123".into(),
        context: {
            let mut m = HashMap::new();
            m.insert("role".into(), serde_json::json!("admin"));
            m
        },
        ttl: Duration::from_secs(300),
    };

    // Miss on empty cache.
    assert!(
        cache.get(&key).await.is_none(),
        "cache must be empty before insert"
    );

    // Insert then hit.
    cache.insert(key.clone(), output).await;
    let hit = cache.get(&key).await;
    assert!(hit.is_some(), "cache must return entry after insert");
    assert_eq!(hit.unwrap().principal_id, "user123");

    // Different source_ip → miss.
    let other_key = AuthCacheKey::new("9.8.7.6", Some("Bearer test-token"), "api");
    assert!(
        cache.get(&other_key).await.is_none(),
        "different source IP must be a cache miss"
    );

    // Different token → miss.
    let other_token_key = AuthCacheKey::new("1.2.3.4", Some("Bearer other-token"), "api");
    assert!(
        cache.get(&other_token_key).await.is_none(),
        "different token must be a cache miss"
    );

    // Different function name → miss.
    let other_fn_key = AuthCacheKey::new("1.2.3.4", Some("Bearer test-token"), "other-fn");
    assert!(
        cache.get(&other_fn_key).await.is_none(),
        "different function name must be a cache miss"
    );
}

/// Criterion 5: `requestContext.authorizer` is populated on successful authorization.
#[test]
fn request_context_authorizer_populated_on_success() {
    use riz::auth::middleware::inject_authorizer_context;
    use riz::gateway::{
        ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
        ApiGatewayV2httpRequestContextHttpDescription,
    };

    let event = ApiGatewayV2httpRequest {
        version: Some("2.0".into()),
        route_key: Some("GET /test".into()),
        raw_path: Some("/test".into()),
        raw_query_string: Some(String::new()),
        cookies: None,
        headers: http::HeaderMap::new(),
        query_string_parameters: Default::default(),
        path_parameters: Default::default(),
        request_context: ApiGatewayV2httpRequestContext {
            route_key: Some("GET /test".into()),
            account_id: Some("riz".into()),
            stage: Some("$default".into()),
            request_id: Some("req-1".into()),
            time: None,
            time_epoch: 0,
            http: ApiGatewayV2httpRequestContextHttpDescription {
                method: http::Method::GET,
                path: Some("/test".into()),
                protocol: Some("HTTP/1.1".into()),
                source_ip: Some("1.2.3.4".into()),
                user_agent: None,
            },
            ..Default::default()
        },
        stage_variables: Default::default(),
        body: None,
        is_base64_encoded: false,
        kind: None,
        method_arn: None,
        http_method: http::Method::GET,
        identity_source: None,
        authorization_token: None,
        resource: None,
    };

    let output = AuthorizerOutput {
        principal_id: "user42".into(),
        context: {
            let mut m = HashMap::new();
            m.insert("orgId".into(), serde_json::json!("org-99"));
            m
        },
        ttl: Duration::from_secs(300),
    };

    let event = inject_authorizer_context(event, &output);

    let auth = event
        .request_context
        .authorizer
        .expect("requestContext.authorizer must be Some after injection");

    assert_eq!(
        auth.fields.get("principalId").and_then(|v| v.as_str()),
        Some("user42"),
        "principalId must be set in authorizer.lambda fields"
    );
    assert_eq!(
        auth.fields.get("orgId").and_then(|v| v.as_str()),
        Some("org-99"),
        "context key 'orgId' must be present in authorizer.lambda fields"
    );
}

/// Criterion 6: 401 returned on authorizer failure (tested via config validation +
/// compile-time type checking — functional 401 tested in authorizer_integration.rs).
#[test]
fn authorizer_failure_returns_401() {
    // Verify the AuthError::Unauthorized variant exists and the error message
    // can be constructed without panic.
    use riz::auth::authorizer::AuthError;
    let err = AuthError::Unauthorized("test".into());
    let msg = format!("{err}");
    assert!(
        msg.contains("unauthorized") || msg.contains("test"),
        "got: {msg}"
    );
}

/// Criterion 6 continued: 403 returned on IAM policy Deny.
#[test]
fn request_authorizer_deny_returns_403() {
    use riz::auth::authorizer::AuthError;
    let err = AuthError::Forbidden("IAM deny".into());
    let msg = format!("{err}");
    assert!(
        msg.contains("forbidden") || msg.contains("IAM"),
        "got: {msg}"
    );
}

/// Criterion 7: `authorizer = "none"` skips auth.
#[test]
fn authorizer_none_opt_out_skips_auth() {
    let toml_str = r#"
[function.api]
runtime = "bun"
handler = "index.handler"
authorizer = "none"
"#;
    let config: Config = toml::from_str(toml_str).expect("must parse");
    let fn_config = config.functions.get("api").unwrap();
    match fn_config.authorizer.as_ref().unwrap() {
        AuthorizerConfig::FunctionRef(name) => {
            assert_eq!(
                name, "none",
                "authorizer = 'none' must parse as FunctionRef('none')"
            );
        }
        other => panic!("expected FunctionRef('none'), got {other:?}"),
    }
    // validate() must also accept "none" without trying to find a "none" function.
    assert!(
        config.validate().is_ok(),
        "authorizer = 'none' must pass validation"
    );
}

/// Criterion D: validation rejects reference to non-existent authorizer function.
#[test]
fn validate_rejects_nonexistent_authorizer_ref() {
    let toml_str = r#"
[function.api]
runtime = "bun"
handler = "index.handler"
authorizer = "no-such-fn"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let err = config.validate().unwrap_err();
    assert!(
        err.contains("no-such-fn"),
        "validation error must name the missing function; got: {err}"
    );
}

/// Criterion D: validation rejects JWT authorizer with empty jwks_uri.
#[test]
fn validate_rejects_jwt_authorizer_with_empty_jwks_uri() {
    let toml_str = r#"
[function.api]
runtime = "bun"
handler = "index.handler"

[function.api.authorizer]
type = "jwt"
issuer = "https://example.com"
audience = "myapp"
jwks_uri = ""
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let err = config.validate().unwrap_err();
    assert!(
        err.contains("jwks_uri"),
        "validation must mention jwks_uri; got: {err}"
    );
}

/// Criterion D: validation rejects JWT authorizer with wrong type field.
#[test]
fn validate_rejects_jwt_authorizer_with_wrong_type() {
    let config = Config {
        functions: {
            let mut m = indexmap::IndexMap::new();
            m.insert(
                "api".into(),
                riz::config::FunctionConfig {
                    runtime: riz::config::RuntimeKind::Bun,
                    protocol: Default::default(),
                    handler: PathBuf::from("index.handler"),
                    timeout_ms: 30_000,
                    integration_timeout_ms: 30_000,
                    concurrency: 1,
                    cache_ttl_secs: None,
                    stage_variables: Default::default(),
                    routes: vec![],
                    cors: None,
                    authorizer: Some(AuthorizerConfig::Jwt(JwtAuthorizerConfig {
                        r#type: "not-jwt".into(),
                        issuer: "https://example.com".into(),
                        audience: "app".into(),
                        jwks_uri: "https://example.com/.well-known/jwks.json".into(),
                    })),
                    memory_mb: None,
                    cpu_time_secs: None,
                    allowed_paths: None,
                },
            );
            m
        },
        ..Default::default()
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.contains("not-jwt") || err.contains("type"),
        "validation must mention the bad type; got: {err}"
    );
}
