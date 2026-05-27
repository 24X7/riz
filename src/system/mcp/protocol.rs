//! JSON-RPC 2.0 protocol types for the MCP server.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// MCP protocol versions this server understands. We echo back the client's
/// version if it appears here; otherwise we respond with SERVER_DEFAULT and let
/// the client decide whether to proceed.
pub(super) const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26"];
pub(super) const SERVER_DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Deserialize)]
pub(super) struct JsonRpcRequest {
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) jsonrpc: String,
    /// Per JSON-RPC 2.0: absent `id` means this is a notification — no response.
    pub(super) id: Option<serde_json::Value>,
    pub(super) method: String,
    #[serde(default)]
    pub(super) params: serde_json::Value,
}

/// Internal error type for the dispatcher — converted to JSON-RPC error
/// shape at the response boundary.
pub(super) struct JsonRpcError {
    pub(super) code: i32,
    pub(super) message: String,
}

#[derive(Serialize)]
pub(super) struct Tool {
    pub(super) name: String,
    pub(super) description: String,
    #[serde(rename = "inputSchema")]
    pub(super) input_schema: serde_json::Value,
}

#[derive(Serialize)]
pub(super) struct ToolsListResult {
    pub(super) tools: Vec<Tool>,
}

#[derive(Serialize)]
pub(super) struct ToolsCallResult {
    pub(super) content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    pub(super) is_error: bool,
}

#[derive(Serialize)]
pub(super) struct ToolContent {
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) text: String,
}

#[derive(Deserialize)]
pub(super) struct ToolsCallParams {
    pub(super) name: String,
    #[serde(default)]
    pub(super) arguments: ToolArguments,
}

#[derive(Deserialize, Default)]
pub(super) struct ToolArguments {
    /// Optional "METHOD /path" selector when the function declares multiple
    /// routes. If omitted, the first declared route is used.
    #[serde(default)]
    pub(super) route: Option<String>,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) headers: HashMap<String, String>,
    #[serde(default, rename = "queryParams")]
    pub(super) query_params: HashMap<String, String>,
    #[serde(default, rename = "pathParams")]
    pub(super) path_params: HashMap<String, String>,
    #[serde(default, rename = "isBase64Encoded")]
    pub(super) is_base64_encoded: bool,
}
