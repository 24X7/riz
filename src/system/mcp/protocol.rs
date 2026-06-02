//! JSON-RPC 2.0 protocol types for the MCP server.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// MCP protocol versions this server understands. We echo back the client's
/// version if it appears here; otherwise we respond with SERVER_DEFAULT and let
/// the client decide whether to proceed.
///
/// Spec history that shapes this list:
///   - 2024-11-05  — original public baseline; still widely deployed in clients.
///   - 2025-03-26  — introduces Streamable HTTP transport, JSON-RPC batching.
///   - 2025-06-18  — REMOVES JSON-RPC batching, adds structured tool output
///                   (`outputSchema` / `structuredContent`), tighter OAuth.
///   - 2025-11-25  — current stable. Adds elicitation, async tasks, enhanced
///                   sampling, Client ID Metadata Documents, the extensions
///                   system, mandatory RFC 8707 Resource Indicators on OAuth.
///
/// Default points at the newest stable; older clients still get their requested
/// version echoed back so legacy negotiation keeps working.
pub(super) const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
];
pub(super) const SERVER_DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

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
    /// MCP 2025-06-18+: declares the JSON Schema of the structured payload
    /// the tool returns alongside its free-text `content`. Clients use this
    /// to validate `structuredContent` on responses. Always-Some for Riz —
    /// every function returns an AWS Lambda response envelope.
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub(super) output_schema: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub(super) struct ToolsListResult {
    pub(super) tools: Vec<Tool>,
}

#[derive(Serialize)]
pub(super) struct ToolsCallResult {
    pub(super) content: Vec<ToolContent>,
    /// MCP 2025-06-18+: typed payload that validates against the tool's
    /// declared `outputSchema`. For Riz this is the parsed Lambda response
    /// (statusCode, headers, body, isBase64Encoded) — clients that want
    /// structured access skip parsing `content[0].text` as JSON.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub(super) structured_content: Option<serde_json::Value>,
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
