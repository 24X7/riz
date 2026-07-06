# WebSocket functions as MCP tools — ephemeral sessions

**Date:** 2026-07-02 · **Status:** approved (user-selected option 1 of 3)

## Problem

WebSocket functions mount as upgrade routes with a `$connect` / `$default` /
`$disconnect` lifecycle — there is no HTTP route for `tools/call` to dispatch
to, so PR #15 removed them from the MCP tool surface entirely. That made the
surface accurate but left half the protocol surface invisible to agents.

## Design

`tools/call("chat-ws", { message, timeout_ms? })` opens a **short-lived
internal WebSocket session** against the function. Every part of the WS
contract behaves normally; the agent just experiences it as a slightly slower
tool.

1. **Connect.** Mint a real `connectionId` and register it in the
   `ConnectionStore`, backed by an in-process collector channel instead of a
   socket. Invoke the function's `$connect` route. A non-2xx `$connect`
   response rejects the session → JSON-RPC error (mirrors a real upgrade
   rejection).
2. **Deliver.** Invoke `$default` with `message` as the event body and the
   minted `connectionId` in the request context — exactly the event a real
   frame produces.
3. **Collect.** Two reply paths, both captured:
   - the `$default` invocation's returned body (riz already relays this to
     the socket on real connections);
   - anything the handler POSTs to `/_riz/connections/{connectionId}` — the
     store entry is real, so pushes land in the collector instead of 410ing.
4. **Return rule (deterministic).** Collect while the `$default` invocation
   runs. When it completes: if ≥1 frame collected, return immediately; if
   zero, keep waiting for async pushes until `timeout_ms` (default 5000,
   capped at the function's `integration_timeout_ms`). Timeout with zero
   frames → empty-frames result (not an error — a silent handler is valid).
5. **Disconnect.** Always invoke `$disconnect` (best-effort) and remove the
   connection from the store, including on timeout and on caller cancel.

### Tool surface

- `tools/list` re-advertises WS functions with
  `inputSchema: { message: string (required), timeout_ms: integer (optional) }`
  and a description that names the session semantics.
- Result: `content` = collected frames as text; `structuredContent` =
  `{ "frames": [...], "connection_id": "..." }`.
- `riz scaffold static` and the `riz://llms.txt` MCP resource re-include WS
  functions with the same session note (PR #15's exclusions are reverted in
  favor of the now-true behavior; its tests are updated to assert the new
  contract).

## Components touched

`src/system/mcp/tools.rs` (dispatch branch for WS), `src/ws/store.rs`
(collector-backed connection kind), `src/ws/management.rs` (pushes route to
collectors transparently), `src/system/mcp/schema.rs` (session input schema),
`src/scaffold.rs` + `src/system/mcp/resources.rs` (re-include), tests
(`mcp_ws_sessions.rs` e2e with a real bun/node WS handler + in-module units).

## Testing

- E2e: boot a WS echo function; `tools/call` returns the echoed frame; a
  handler that pushes via `@connections` has its push collected; `$connect`
  rejection → error; silent handler → empty frames after a short timeout;
  `$disconnect` observed (handler writes a marker file / logs assertion).
- Unit: return rule (frames-then-return, zero-frames-waits), timeout capping,
  store cleanup on every exit path (no leaked connections).

## Out of scope

Multi-turn sessions across tool calls (each call is one session), binary
frames in results (reported as base64 with a mimeType note), streaming
partial frames over MCP progress notifications (future).
