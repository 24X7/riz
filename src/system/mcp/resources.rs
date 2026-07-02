//! MCP resources: a live riz instance describing itself to agents.
//!
//! Two resources, both derived from live state (they track hot-swaps and
//! config reloads, unlike files on disk):
//!
//!   riz://registry  — the function registry, same JSON as GET /_riz/registry
//!   riz://llms.txt  — the agent-readable when-to-use card: the same tool
//!                     surface `tools/list` advertises (WS functions excluded,
//!                     same rule), plus how to call the MCP endpoint. The
//!                     static-file twin is `riz scaffold static`.

use crate::state::FunctionKind;

use super::protocol::JsonRpcError;
use super::McpHandler;

/// MCP spec error code for "resource not found".
const RESOURCE_NOT_FOUND: i32 = -32002;

const REGISTRY_URI: &str = "riz://registry";
const LLMS_URI: &str = "riz://llms.txt";

impl McpHandler {
    pub(super) async fn resources_list_value(&self) -> Result<serde_json::Value, JsonRpcError> {
        Ok(serde_json::json!({
            "resources": [
                {
                    "uri": REGISTRY_URI,
                    "name": "registry",
                    "description": "Live function registry: every mounted function with routes, runtime, and pool settings (same JSON as GET /_riz/registry).",
                    "mimeType": "application/json",
                },
                {
                    "uri": LLMS_URI,
                    "name": "llms.txt",
                    "description": "The agent-readable when-to-use card for this instance: every callable tool with routes and description, and how to call the MCP endpoint.",
                    "mimeType": "text/markdown",
                },
            ]
        }))
    }

    pub(super) async fn resources_read_value(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, JsonRpcError> {
        let uri = params
            .get("uri")
            .and_then(|u| u.as_str())
            .ok_or_else(|| JsonRpcError {
                code: -32602,
                message: "missing required parameter: uri".into(),
            })?;
        let (mime, text) = match uri {
            REGISTRY_URI => (
                "application/json",
                crate::system::registry::registry_json(&self.riz_state)
                    .await
                    .to_string(),
            ),
            LLMS_URI => ("text/markdown", self.render_llms_txt().await),
            other => {
                return Err(JsonRpcError {
                    code: RESOURCE_NOT_FOUND,
                    message: format!("resource not found: {other}"),
                })
            }
        };
        Ok(serde_json::json!({
            "contents": [{ "uri": uri, "mimeType": mime, "text": text }]
        }))
    }

    /// Render the live llms.txt: mirrors `riz scaffold static`'s generator but
    /// over the LIVE function set — hot-swapped functions appear, removed ones
    /// don't, and WebSocket functions are excluded exactly like `tools/list`.
    async fn render_llms_txt(&self) -> String {
        let functions = self.riz_state.functions.read().await;
        let tools: Vec<_> = functions
            .iter()
            .map(|(_, f)| f)
            .filter(|f| matches!(f.kind, FunctionKind::User) && !super::tools::is_websocket(f))
            .collect();

        let mut out = String::new();
        out.push_str("# riz instance\n\n");
        out.push_str(&format!(
            "> A self-hosted riz runtime exposing {} function{} as typed MCP tools \
             at `/_riz/mcp`. Every HTTP handler below is callable by an agent — zero glue.\n\n",
            tools.len(),
            if tools.len() == 1 { "" } else { "s" }
        ));
        out.push_str("## Tools\n\n");
        for f in &tools {
            let description = f
                .config
                .as_ref()
                .and_then(|c| c.mcp.as_ref())
                .and_then(|m| m.description.clone())
                .unwrap_or_else(|| match &f.config {
                    Some(c) => format!(
                        "Invoke function `{}` ({} runtime). Routes: [{}]",
                        f.name,
                        c.runtime.as_str(),
                        f.routes.join(", "),
                    ),
                    None => format!("Invoke {}", f.name),
                });
            out.push_str(&format!("### {}\n\n", f.name));
            if let Some(c) = &f.config {
                out.push_str(&format!("- runtime: `{}`\n", c.runtime.as_str()));
            }
            let routes = f
                .routes
                .iter()
                .map(|r| format!("`{r}`"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("- routes: {routes}\n"));
            out.push_str(&format!("- {description}\n\n"));
        }
        out.push_str("## MCP endpoint\n\n");
        out.push_str(
            "- `POST /_riz/mcp` — JSON-RPC 2.0 over Streamable HTTP.\n\
             - Methods: `initialize`, `tools/list`, `tools/call`, `resources/list`, `resources/read`.\n\
             - Wire up: `claude mcp add riz --transport http <this-host>/_riz/mcp`.\n",
        );
        out
    }
}
