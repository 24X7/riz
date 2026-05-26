//! Riz system functions mounted under /_riz/*.
//! Each handler implements LambdaHandler and reads from RizState.

pub mod health;
pub mod mcp;
pub mod metrics;
pub mod registry;

/// Derive a stable, MCP-compatible tool name from a route_key like "GET /api/users/:id".
/// Result: "GET_api_users_id". Runs of separators collapse to a single underscore.
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
