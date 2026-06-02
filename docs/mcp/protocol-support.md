# MCP — Protocol Support

Authoritative compatibility surface for Riz's MCP server at `/_riz/mcp`.

## Version matrix

| Spec version | Status | Notes |
|---|---|---|
| **2025-11-25** | ✅ **default** | Current stable. Riz's `initialize` advertises this when the client omits `protocolVersion`. |
| 2025-06-18 | ✅ negotiated | Echoed back when requested. Adds structured tool output + tighter OAuth (Riz's bearer path is honored; OAuth 2.1 path is roadmap). |
| 2025-03-26 | ✅ negotiated | Echoed back when requested. Introduces Streamable HTTP transport — Riz supports it. Still accepts JSON-RPC batch (removed in 2025-06-18). |
| 2024-11-05 | ✅ negotiated | Original baseline. Riz still accepts batch arrays from these clients. |

If a client requests a version Riz doesn't recognize, `initialize` responds with the server default (2025-11-25) and the client decides whether to proceed.

Source: `src/system/mcp/protocol.rs` (`SUPPORTED_PROTOCOL_VERSIONS`).

## Method matrix

| JSON-RPC method | Status | Implementation |
|---|---|---|
| `initialize` | ✅ | Version negotiation + capability handshake. Echoes back the client's `protocolVersion` if supported, else server default. |
| `notifications/initialized` | ✅ | Accepted silently per JSON-RPC notification semantics. |
| `ping` | ✅ | Returns `{}`. |
| `tools/list` | ✅ | Enumerates every user function in `riz.toml` as a tool. System endpoints (`/_riz/*`) are filtered out. |
| `tools/call` | ✅ | Builds an AWS API Gateway v2 event from the arguments, dispatches through Riz's Router. Returns both `content[]` (back-compat) and `structuredContent` (2025-06-18+). |
| `resources/list` | ✅ stub | Returns `{ "resources": [] }`. Riz doesn't expose resources today. |
| `resources/templates/list` | ✅ stub | Returns `{ "resourceTemplates": [] }`. |
| `prompts/list` | ✅ stub | Returns `{ "prompts": [] }`. |
| `elicitation/create` | ❌ | Riz is a server; doesn't initiate elicitations. Accepting the **client** capability silently (so 2025-11-25 clients don't break) is implemented. |
| `completion/complete` | ❌ | Not implemented. |
| `logging/setLevel` | ❌ | Not implemented. |
| `sampling/createMessage` | ❌ | Riz doesn't drive sampling. |

## Transport matrix

| Transport | Status | Notes |
|---|---|---|
| **Streamable HTTP** (POST) | ✅ | Single endpoint, JSON-RPC body in, JSON response out. Default for current clients. |
| Streamable HTTP (GET) | ✅ 405 | Returns `405 Method Not Allowed` with `Allow: POST`. Spec-correct response when the server doesn't push server-initiated streams. Lets clients distinguish "transport supported, GET unused" from "wrong endpoint." |
| HTTP+SSE (legacy, pre-2025-03-26) | ❌ | Deprecated by spec. Not supported. |
| stdio | ❌ | Not the deployment model. Riz is a long-running HTTP server, not a spawn-per-call subprocess. |

## Capability matrix

What Riz advertises on `initialize` under `capabilities`:

| Capability | Advertised | Notes |
|---|---|---|
| `tools` | ✅ `{ "listChanged": false }` | Tool set is static for a given `riz.toml` revision. After a hot-reload that adds/removes functions, the tool set changes — clients can re-issue `tools/list`. |
| `resources` | ❌ | Stubs only; not advertised. |
| `prompts` | ❌ | Stubs only; not advertised. |
| `logging` | ❌ | Not implemented. |
| `experimental` | ❌ | None today. |

Client capabilities Riz **accepts** but doesn't act on (the server side is a no-op):

| Client capability | Behavior |
|---|---|
| `roots` | Accepted; ignored. |
| `sampling` | Accepted; Riz doesn't request sampling. |
| `elicitation` | Accepted; Riz doesn't initiate elicitations. |

## Auth matrix

| Mechanism | Status | Configuration |
|---|---|---|
| **Bearer token** | ✅ default | `RIZ_AUTH_BEARER_TOKEN` env or `[auth] bearer_token` in `riz.toml`. Constant-time comparison via the `subtle` crate. |
| No auth | ✅ | Default when neither env nor config is set. Suitable for local dev only. |
| OAuth 2.1 + RFC 8707 Resource Indicators | 🗓 roadmap | Planned. The spec mandates this path for 2025-11-25 OAuth deployments. Bearer-token will remain the default. |

`/_riz/health` is always open for liveness probes; every other `/_riz/*` endpoint (including `/_riz/mcp`) is gated when a token is configured.

## What's intentionally absent

- **JSON-RPC batch deprecation.** Batching was removed in 2025-06-18. Riz still accepts batches when sent by older clients (2024-11-05 / 2025-03-26) but does not encourage their use. New client code should send single requests.
- **Server-initiated SSE / Tasks (2026 RC).** The 2026-07-28 release candidate introduces an async-task extension. Riz tracks the RC but ships the 2025-11-25 surface only.
- **MCP Apps (server-rendered UIs).** Also a 2026-RC extension; out of scope for v0.1.

## Regression guarantee

Every entry above marked ✅ has a corresponding test in `src/system/mcp/mod.rs::tests` or `tests/system_functions_integration.rs`. Specifically:

| Claim | Test name |
|---|---|
| Default protocol version is 2025-11-25 | `initialize_with_no_version_defaults_to_2025_11_25` |
| 2025-06-18 negotiation | `initialize_echoes_2025_06_18_when_requested` |
| 2025-11-25 negotiation | `initialize_echoes_2025_11_25_when_requested` |
| `tools/list` declares `outputSchema` | `tools_list_advertises_lambda_output_schema` |
| `tools/call` returns `structuredContent` | `tools_call_returns_structured_content_with_lambda_envelope` |
| GET returns 405 + Allow: POST | `get_on_mcp_endpoint_returns_405_with_allow_post` |
| GET + POST routes registered | `handler_advertises_both_get_and_post_routes` |
| Elicitation client capability accepted silently | `initialize_accepts_client_elicitation_capability_silently` |
| Bearer-token gating | `mcp_returns_401_when_token_required_and_missing` + variants |
| JSON-RPC notification handling | `notification_without_id_returns_204_no_content` |
| Batch handling (legacy) | `batch_request_returns_array_of_responses` + variants |

Run with `cargo nextest run mcp` to verify any of these locally.
