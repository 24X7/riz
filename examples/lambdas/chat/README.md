# chat — WebSocket lifecycle + @connections push (Bun)

A WebSocket handler that receives the three AWS lifecycle events and echoes
each message back to the sender. It is the Bun member of a three-runtime WS
parity set (`chat`, `chat-python`, `chat-rust`).

**Capability:** `protocol = "websocket"`; `$connect` / `$default` /
`$disconnect` events shaped as AWS `ApiGatewayWebsocketProxyRequest`;
server→client push via the local `@connections` management endpoint.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## How the echo works

To send a message *to* a connected client, the handler POSTs to riz's local
equivalent of the AWS API Gateway `@connections` API:

```
POST http://localhost:3000/_riz/connections/{connectionId}
body: <raw bytes to deliver>
```

## Wiring (`examples/riz.all.toml`)

```toml
[function.chat]
protocol = "websocket"
runtime = "bun"
handler = "./examples/lambdas/chat/index.handler"
[[function.chat.routes]]
path = "/chat"
method = "ANY"
```

## Run

```bash
echo hello | websocat ws://127.0.0.1:3000/chat
# echo: hello
```
