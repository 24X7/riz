# chat-python — WebSocket lifecycle + @connections push (Python)

The Python member of the WebSocket parity set (`chat`, `chat-python`,
`chat-rust`). Same behaviour as the Bun `chat` example: receives `$connect` /
`$default` / `$disconnect` and echoes each message back via the `@connections`
management endpoint.

**Capability:** WebSocket handler in Python using only the standard library
(`urllib`) — no SDK.
**Runtime:** Python · **Handler:** `main.py` → `lambda_handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.chat-python]
protocol = "websocket"
runtime = "python"
handler = "./examples/lambdas/chat-python/main.lambda_handler"
[[function.chat-python.routes]]
path = "/chat-python"
method = "ANY"
```

The `@connections` base URL defaults to `http://localhost:3000` and can be
overridden with `RIZ_TEST_BASE_URL` (used by integration tests that bind to an
ephemeral port).

## Run

```bash
echo hello | websocat ws://127.0.0.1:3000/chat-python
# echo: hello
```
