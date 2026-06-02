# MCP — Getting Started

This page is the 60-second path from "I have a `riz.toml`" to "Claude Code is calling my Lambdas."

If you don't have a Riz project yet:

```bash
riz init typescript-http my-app
cd my-app
riz run
```

`riz run` starts the runtime on `http://localhost:3000` and mounts the MCP server at `/_riz/mcp`. Every function in your `riz.toml` becomes an MCP tool automatically — no SDK, no wrappers, no extra config.

---

## Claude Code

```bash
claude mcp add riz-local \
    --transport http \
    http://localhost:3000/_riz/mcp
```

That's it. Within Claude Code, your functions show up as tools. To verify:

```
/mcp                              # shows riz-local as a connected server
@riz-local                        # tab-completes available tools
```

When Claude calls one of your tools, the call goes through the same Router that serves real HTTP traffic — same handler code, same Lambda event shape, same response.

### With bearer-token auth

If you've set `RIZ_AUTH_BEARER_TOKEN` or `[auth] bearer_token` in `riz.toml`, pass the token via the `Authorization` header. Claude Code reads it from the env or the per-server config:

```bash
claude mcp add riz-local \
    --transport http \
    --header "Authorization: Bearer $RIZ_AUTH_BEARER_TOKEN" \
    http://localhost:3000/_riz/mcp
```

`/_riz/health` always stays open for liveness probes; everything else under `/_riz/*` (including `/_riz/mcp`) is gated.

---

## Cursor

In Cursor's settings → MCP → Add Server:

```json
{
  "mcpServers": {
    "riz-local": {
      "url": "http://localhost:3000/_riz/mcp",
      "transport": "http"
    }
  }
}
```

Bearer-protected setups add the header inline:

```json
{
  "mcpServers": {
    "riz-local": {
      "url": "http://localhost:3000/_riz/mcp",
      "transport": "http",
      "headers": {
        "Authorization": "Bearer ${env:RIZ_AUTH_BEARER_TOKEN}"
      }
    }
  }
}
```

---

## Any other MCP client

The endpoint is `http://<host>:<port>/_riz/mcp`. Transport is **Streamable HTTP** (MCP 2025-03-26+). Send a POST with a JSON-RPC 2.0 body; receive a JSON or `text/event-stream` response.

```bash
# tools/list
curl -s -X POST http://localhost:3000/_riz/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq

# initialize
curl -s -X POST http://localhost:3000/_riz/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}' | jq
```

GET on `/_riz/mcp` returns `405 Method Not Allowed` with `Allow: POST` — Riz is fundamentally request/response and doesn't push server-initiated SSE streams. Clients can use this to distinguish "MCP endpoint exists, GET unused" from "endpoint missing."

---

## What a tool looks like

For a `riz.toml` like:

```toml
[function.api]
runtime = "bun"
handler = "src/api/index.handler"

[[function.api.routes]]
path   = "/api/{id}"
method = "GET"
```

`tools/list` returns:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [
      {
        "name": "api",
        "description": "Invoke function `api` (bun runtime). Routes: [GET /api/{id}]",
        "inputSchema": {
          "type": "object",
          "properties": {
            "route":           { "type": "string" },
            "body":            { "type": "string" },
            "headers":         { "type": "object" },
            "queryParams":     { "type": "object" },
            "pathParams":      { "type": "object" },
            "isBase64Encoded": { "type": "boolean" }
          }
        },
        "outputSchema": {
          "type": "object",
          "properties": {
            "statusCode":      { "type": "integer" },
            "headers":         { "type": "object" },
            "body":            { "type": "string" },
            "isBase64Encoded": { "type": "boolean" }
          },
          "required": ["statusCode"]
        }
      }
    ]
  }
}
```

The `outputSchema` is the AWS Lambda response envelope. MCP 2025-06-18+ clients use it to validate `structuredContent` on `tools/call` responses without re-parsing `content[0].text`.

---

## Calling a tool

```bash
curl -s -X POST http://localhost:3000/_riz/mcp \
  -H 'content-type: application/json' \
  -d '{
    "jsonrpc":"2.0","id":1,"method":"tools/call",
    "params":{
      "name":"api",
      "arguments":{ "pathParams": {"id": "42"} }
    }
  }' | jq
```

Response (truncated):

```json
{
  "result": {
    "content": [
      { "type": "text", "text": "{\"statusCode\":200,\"body\":\"{\\\"id\\\":\\\"42\\\",\\\"name\\\":\\\"Account 42\\\"}\"}" }
    ],
    "structuredContent": {
      "statusCode": 200,
      "body": "{\"id\":\"42\",\"name\":\"Account 42\"}",
      "headers": { "content-type": "application/json" }
    },
    "isError": false
  }
}
```

`content[]` is the back-compat path for pre-2025-06-18 clients (the response serialized as a single text block). `structuredContent` is the typed payload for current clients — same data, no re-parse needed.

---

## Multi-route functions

If a function declares multiple routes, pick which one a tool call hits via the `route` argument:

```toml
[function.users]
runtime = "bun"
handler = "src/users/index.handler"

[[function.users.routes]]
path = "/users"
method = "POST"

[[function.users.routes]]
path = "/users/{id}"
method = "DELETE"
```

```json
{
  "method": "tools/call",
  "params": {
    "name": "users",
    "arguments": {
      "route": "DELETE /users/{id}",
      "pathParams": { "id": "42" }
    }
  }
}
```

Omit `route` and Riz uses the function's first declared route.

---

## See also

- [`docs/mcp/protocol-support.md`](./protocol-support.md) — exact spec-version + capability matrix
- [Model Context Protocol — 2025-11-25 spec](https://modelcontextprotocol.io/specification/2025-11-25)
- Bearer auth: `RIZ_AUTH_BEARER_TOKEN` env or `[auth] bearer_token` in `riz.toml`
