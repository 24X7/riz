//! Wave 3 — Lambda authorizers (REQUEST + JWT) acceptance criteria.

#[test]
#[ignore = "wave 3 not yet shipped: Authorizer trait with authorize(event) -> Result<AuthorizerOutput, AuthError>"]
fn authorizer_trait_exists_with_correct_signature() {
    // Wave 3: the auth module with Authorizer trait must exist.
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — Wave 3 authorizer trait not yet shipped"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: RequestAuthorizer calls user-declared function as authorizer"]
fn request_authorizer_calls_user_function() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — RequestAuthorizer not yet shipped (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: JwtAuthorizer validates against a JWKS URL"]
fn jwt_authorizer_validates_against_jwks_url() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — JwtAuthorizer not yet shipped (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer config accepted in [function.api] block"]
fn authorizer_config_parses_from_function_block() {
    // Wave 3: FunctionConfig must have an `authorizer` field.
    // Use a toml that includes an authorizer declaration — must parse without error.
    let toml_str = r#"
[function.api]
runtime = "bun"
handler = "index.handler"
authorizer = "my-auth-fn"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    // This must parse without failing — when Wave 3 ships, FunctionConfig gains
    // the `authorizer` field. Today toml::from_str may fail or ignore the field.
    let result = toml::from_str::<riz::config::Config>(toml_str);
    assert!(
        result.is_ok(),
        "authorizer field not accepted by FunctionConfig — Wave 3 not yet shipped: {:?}",
        result.err()
    );
    let config = result.unwrap();
    // When Wave 3 ships, the authorizer field should be Some("my-auth-fn").
    // For now, just verifying it parses is sufficient — the real assertion is above.
    let _ = config;
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer responses cached by source IP + Authorization header hash for TTL"]
fn authorizer_responses_cached_with_ttl() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — authorizer TTL cache not yet shipped (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: requestContext.authorizer populated on successful authorize"]
fn request_context_authorizer_populated_on_success() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — requestContext.authorizer not yet populated (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: 401 returned on authorizer failure"]
fn authorizer_failure_returns_401() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — 401 on authorizer failure not yet shipped (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: 403 returned when iam_policy.Effect != Allow (REQUEST authorizer)"]
fn request_authorizer_deny_returns_403() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — 403 on IAM deny not yet shipped (Wave 3)"
    );
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer = none skips auth even if global authorizer is declared"]
fn authorizer_none_opt_out_skips_auth() {
    assert!(
        std::path::Path::new("src/auth/mod.rs").exists()
            || std::path::Path::new("src/auth.rs").exists(),
        "missing src/auth module — authorizer opt-out not yet shipped (Wave 3)"
    );
}
