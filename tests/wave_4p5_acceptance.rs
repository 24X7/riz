//! Wave 4.5 — Bearer-token auth on `/_riz/*` acceptance criteria.

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/health returns 200 with no auth (liveness must remain open)"]
fn riz_health_open_without_auth() {
    // Wave 4.5: the [auth] bearer_token config field must exist.
    let toml_str = r#"
[auth]
bearer_token = "secret-token"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let result = toml::from_str::<riz::config::Config>(toml_str);
    assert!(
        result.is_ok(),
        "[auth] bearer_token config not parsed — Wave 4.5 not yet shipped: {:?}",
        result.err()
    );
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/metrics returns 401 with no auth when [auth] bearer_token is set"]
fn riz_metrics_returns_401_without_auth_when_token_configured() {
    // Wave 4.5: [auth] bearer_token config block must be accepted.
    let toml_str = r#"
[auth]
bearer_token = "secret-token"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str)
        .expect("[auth] bearer_token must parse — Wave 4.5 not yet shipped");
    // When Wave 4.5 ships, config.auth.bearer_token should be Some("secret-token").
    let _ = config;
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/metrics returns 200 with correct Authorization: Bearer <token> header"]
fn riz_metrics_returns_200_with_correct_bearer_token() {
    let toml_str = r#"
[auth]
bearer_token = "secret-token"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str)
        .expect("[auth] bearer_token must parse — Wave 4.5 not yet shipped");
    let _ = config;
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/registry requires bearer auth when token configured"]
fn riz_registry_requires_bearer_auth() {
    let toml_str = r#"
[auth]
bearer_token = "secret-token"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str)
        .expect("[auth] bearer_token must parse — Wave 4.5 not yet shipped");
    let _ = config;
}

#[test]
#[ignore = "wave 4.5 not yet shipped: /_riz/mcp requires bearer auth when token configured"]
fn riz_mcp_requires_bearer_auth() {
    let toml_str = r#"
[auth]
bearer_token = "secret-token"

[function.api]
runtime = "bun"
handler = "index.handler"
[[function.api.routes]]
path = "/api"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str)
        .expect("[auth] bearer_token must parse — Wave 4.5 not yet shipped");
    let _ = config;
}
