//! Wave 4.5 — Bearer-token auth on `/_riz/*` acceptance criteria.

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/health returns 200 with no auth (liveness must remain open)"]
fn riz_health_open_without_auth() {
    // Implementer fills in during Wave 4.5 tasks.
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/metrics returns 401 with no auth when [auth] bearer_token is set"]
fn riz_metrics_returns_401_without_auth_when_token_configured() {
    // Implementer fills in during Wave 4.5 tasks.
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/metrics returns 200 with correct Authorization: Bearer <token> header"]
fn riz_metrics_returns_200_with_correct_bearer_token() {
    // Implementer fills in during Wave 4.5 tasks.
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/registry requires bearer auth when token configured"]
fn riz_registry_requires_bearer_auth() {
    // Implementer fills in during Wave 4.5 tasks.
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/mcp requires bearer auth when token configured"]
fn riz_mcp_requires_bearer_auth() {
    // Implementer fills in during Wave 4.5 tasks.
}
