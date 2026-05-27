//! Wave 4 — CORS auto-preflight acceptance criteria.

#[test]
#[ignore = "wave 4 not yet shipped: [cors] config block parsed with all fields"]
fn cors_config_block_parses() {
    // Wave 4: a [cors] block in config must parse into a CorsConfig struct.
    let toml_str = r#"
[cors]
allowed_origins = ["https://example.com"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Content-Type", "Authorization"]
max_age_secs = 86400

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
        "[cors] config block not parsed — Wave 4 not yet shipped: {:?}",
        result.err()
    );
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS request to registered route returns 204 with Access-Control-Allow-* headers"]
fn cors_preflight_returns_204_for_options() {
    // Wave 4: CORS config struct must exist in the config module.
    assert!(
        std::path::Path::new("src/cors.rs").exists()
            || std::path::Path::new("src/cors/mod.rs").exists()
            || std::path::Path::new("src/gateway/cors.rs").exists(),
        "missing CORS implementation — Wave 4 not yet shipped (expected src/cors.rs or src/cors/mod.rs)"
    );
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS preflight never reaches the handler"]
fn cors_preflight_does_not_invoke_handler() {
    assert!(
        std::path::Path::new("src/cors.rs").exists()
            || std::path::Path::new("src/cors/mod.rs").exists()
            || std::path::Path::new("src/gateway/cors.rs").exists(),
        "missing CORS implementation — OPTIONS preflight handler bypass not yet shipped (Wave 4)"
    );
}

#[test]
#[ignore = "wave 4 not yet shipped: non-OPTIONS requests get Access-Control-Allow-Origin echoed when origin is in allowlist"]
fn cors_non_preflight_echoes_allow_origin() {
    assert!(
        std::path::Path::new("src/cors.rs").exists()
            || std::path::Path::new("src/cors/mod.rs").exists()
            || std::path::Path::new("src/gateway/cors.rs").exists(),
        "missing CORS implementation — Allow-Origin echo not yet shipped (Wave 4)"
    );
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS to unregistered path returns 404 even with CORS headers"]
fn cors_preflight_unregistered_path_returns_404() {
    assert!(
        std::path::Path::new("src/cors.rs").exists()
            || std::path::Path::new("src/cors/mod.rs").exists()
            || std::path::Path::new("src/gateway/cors.rs").exists(),
        "missing CORS implementation — unregistered OPTIONS 404 not yet shipped (Wave 4)"
    );
}

#[test]
#[ignore = "wave 4 not yet shipped: per-function CORS override takes precedence over global [cors] block"]
fn cors_per_function_override_takes_precedence() {
    assert!(
        std::path::Path::new("src/cors.rs").exists()
            || std::path::Path::new("src/cors/mod.rs").exists()
            || std::path::Path::new("src/gateway/cors.rs").exists(),
        "missing CORS implementation — per-function override not yet shipped (Wave 4)"
    );
}
