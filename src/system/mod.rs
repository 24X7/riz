//! Riz system functions mounted under /_riz/*.
//! Each handler implements LambdaHandler and reads from RizState.

pub mod a2a;
pub mod health;
pub mod mcp;
pub mod metrics;
pub mod openai_compat;
pub mod registry;

/// The ONE table of every HTTP surface riz mounts outside the user function
/// set — probes, `/_riz/*` admin, and the conditional gateway / A2A surfaces.
/// Both the boot-time `RizState` registration (main.rs — feeds the `--dev`
/// TUI and `GET /_riz/registry`) and `riz routes` print from here, so the
/// reported surface can never drift from what's actually mounted. Add a new
/// axum route → add it here, and every reporting surface follows.
pub fn system_surface(config: &crate::config::Config) -> Vec<(&'static str, Vec<String>)> {
    let mut out: Vec<(&'static str, Vec<String>)> = vec![
        (
            "_riz_probes",
            vec!["GET /health".into(), "GET /ready".into()],
        ),
        ("_riz_health", vec!["GET /_riz/health".into()]),
        ("_riz_metrics", vec!["GET /_riz/metrics".into()]),
        ("_riz_registry", vec!["GET /_riz/registry".into()]),
        (
            "_riz_mcp",
            vec!["POST /_riz/mcp".into(), "GET /_riz/mcp".into()],
        ),
        (
            "_riz_connections",
            vec![
                "GET /_riz/connections".into(),
                "GET /_riz/connections/{id}".into(),
                "POST /_riz/connections/{id}".into(),
                "DELETE /_riz/connections/{id}".into(),
            ],
        ),
        ("_riz_deploy", vec!["POST /deploy".into()]),
        ("_riz_cache", vec!["POST /cache/invalidate".into()]),
    ];
    if config.gateway.enabled() {
        out.push((
            "_riz_gateway",
            vec![
                "POST /_riz/v1/chat/completions".into(),
                "POST /_riz/v1/embeddings".into(),
                "GET /_riz/v1/models".into(),
                "GET /_riz/v1/usage".into(),
            ],
        ));
    }
    if config.agent.is_some() {
        out.push((
            "_riz_a2a",
            vec![
                "POST /_riz/a2a".into(),
                "GET /.well-known/agent-card.json".into(),
            ],
        ));
    }
    out
}

/// Derive a stable, MCP-compatible tool name from a route_key like "GET /api/users/:id".
/// Result: "GET_api_users_id". Runs of separators collapse to a single underscore.
#[allow(dead_code)]
pub fn mcp_tool_name(route_key: &str) -> String {
    let mut out = String::with_capacity(route_key.len());
    let mut last_was_sep = false;
    for c in route_key.chars() {
        match c {
            ' ' | '/' => {
                if !last_was_sep {
                    out.push('_');
                    last_was_sep = true;
                }
            }
            ':' => continue,
            _ => {
                out.push(c);
                last_was_sep = false;
            }
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_strips_colon_replaces_slash() {
        assert_eq!(mcp_tool_name("GET /api"), "GET_api");
        assert_eq!(mcp_tool_name("POST /accounts/:id"), "POST_accounts_id");
        assert_eq!(mcp_tool_name("GET /a/b/c"), "GET_a_b_c");
    }
}
