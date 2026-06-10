# agent-tools — functions that become agent-callable MCP tools (Bun)

Three small, side-effect-light handlers that an LLM agent calls over riz's
MCP endpoint. Each route auto-becomes one MCP tool at `/_riz/mcp` the moment
`riz run` boots — the tool name is the riz.toml function name.

**Capability:** the agent substrate — your functions are agent tools, no SDK
code required.
**Runtime:** Bun · **Handlers:** `index.ts` → `lookupOrder` / `listInventory` / `createTicket`

| riz function     | route               | MCP tool (`mcp__riz__…`) | what it does                       |
| ---------------- | ------------------- | ------------------------- | ---------------------------------- |
| `lookup_order`   | `GET  /orders/{id}` | `mcp__riz__lookup_order`  | order status + `delayed` flag      |
| `list_inventory` | `GET  /inventory`   | `mcp__riz__list_inventory`| current stock levels               |
| `create_ticket`  | `POST /tickets`     | `mcp__riz__create_ticket` | open a support ticket (in-memory)  |

Data is deterministic in-memory seed data. Order `1042` is delayed on
purpose — it's the canonical "look up 1042, open a ticket if it's delayed"
demo path.

## Wiring (`examples/riz.agent.toml`)

```toml
[function.lookup_order]
runtime = "bun"
handler = "./examples/lambdas/agent-tools/index.lookupOrder"
[[function.lookup_order.routes]]
path = "/orders/{id}"
method = "GET"
# … list_inventory and create_ticket likewise
```

## Run

```bash
riz --config examples/riz.agent.toml run

curl http://127.0.0.1:3000/orders/1042                                    # delayed: true
curl http://127.0.0.1:3000/inventory                                      # stock levels
curl -X POST -H 'content-type: application/json' \
  -d '{"orderId":"1042","reason":"delayed 9 days"}' \
  http://127.0.0.1:3000/tickets                                           # 201, ticket open
```

## As MCP tools

```bash
# discover the tools the agent sees
curl -s -X POST http://127.0.0.1:3000/_riz/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

# call lookup_order exactly as the agent would
curl -s -X POST http://127.0.0.1:3000/_riz/mcp -d '{
  "jsonrpc":"2.0","id":2,"method":"tools/call",
  "params":{"name":"lookup_order","arguments":{"pathParams":{"id":"1042"}}}
}'
```

The flagship Claude Agent SDK demo that drives these tools lives in
[`examples/agent-sdk/`](../../agent-sdk/). The substrate is proven in CI
(no API key) by `tests/examples_agent.rs`.

**State note:** stores are module-level, reset on process replacement — a
local-dev illustration, not a durable datastore.
