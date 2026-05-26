//! Wave 4 — CORS auto-preflight acceptance criteria.

#[test]
#[ignore = "wave 4 not yet shipped: [cors] config block parsed with all fields"]
fn cors_config_block_parses() {
    // Implementer fills in during Wave 4 tasks.
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS request to registered route returns 204 with Access-Control-Allow-* headers"]
fn cors_preflight_returns_204_for_options() {
    // Implementer fills in during Wave 4 tasks.
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS preflight never reaches the handler"]
fn cors_preflight_does_not_invoke_handler() {
    // Implementer fills in during Wave 4 tasks.
}

#[test]
#[ignore = "wave 4 not yet shipped: non-OPTIONS requests get Access-Control-Allow-Origin echoed when origin is in allowlist"]
fn cors_non_preflight_echoes_allow_origin() {
    // Implementer fills in during Wave 4 tasks.
}

#[test]
#[ignore = "wave 4 not yet shipped: OPTIONS to unregistered path returns 404 even with CORS headers"]
fn cors_preflight_unregistered_path_returns_404() {
    // Implementer fills in during Wave 4 tasks.
}

#[test]
#[ignore = "wave 4 not yet shipped: per-function CORS override takes precedence over global [cors] block"]
fn cors_per_function_override_takes_precedence() {
    // Implementer fills in during Wave 4 tasks.
}
