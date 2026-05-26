//! Wave 3 — Lambda authorizers (REQUEST + JWT) acceptance criteria.

#[test]
#[ignore = "wave 3 not yet shipped: Authorizer trait with authorize(event) -> Result<AuthorizerOutput, AuthError>"]
fn authorizer_trait_exists_with_correct_signature() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: RequestAuthorizer calls user-declared function as authorizer"]
fn request_authorizer_calls_user_function() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: JwtAuthorizer validates against a JWKS URL"]
fn jwt_authorizer_validates_against_jwks_url() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer config accepted in [function.api] block"]
fn authorizer_config_parses_from_function_block() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer responses cached by source IP + Authorization header hash for TTL"]
fn authorizer_responses_cached_with_ttl() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: requestContext.authorizer populated on successful authorize"]
fn request_context_authorizer_populated_on_success() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: 401 returned on authorizer failure"]
fn authorizer_failure_returns_401() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: 403 returned when iam_policy.Effect != Allow (REQUEST authorizer)"]
fn request_authorizer_deny_returns_403() {
    // Implementer fills in during Wave 3 tasks.
}

#[test]
#[ignore = "wave 3 not yet shipped: authorizer = none skips auth even if global authorizer is declared"]
fn authorizer_none_opt_out_skips_auth() {
    // Implementer fills in during Wave 3 tasks.
}
