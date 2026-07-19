# chat-rust — WebSocket lifecycle + @connections push (Rust)

The Rust member of the WebSocket parity set (`chat`, `chat-python`,
`chat-rust`). Built on the official `lambda_runtime` crate, compiled to a
static binary riz drives through its per-worker AWS Lambda Runtime API. Same
behaviour as the Bun `chat` example.

**Capability:** WebSocket handler in Rust using the typed
`ApiGatewayWebsocketProxyRequest` event; echoes messages via `@connections`.
**Runtime:** Rust · **Handler:** compiled binary `target/release/chat-rust`

## Build & wiring

```bash
cargo build --release --manifest-path tests/fixtures/parity/chat-rust/Cargo.toml
# → target/release/chat-rust
```

```toml
[function.chat-rust]
protocol = "websocket"
runtime = "rust"
handler = "./target/release/chat-rust"
[[function.chat-rust.routes]]
path = "/chat-rust"
method = "ANY"
```

`RIZ_TEST_BASE_URL` overrides the `@connections` base for tests on ephemeral
ports.

## Run

```bash
echo hello | websocat ws://127.0.0.1:3000/chat-rust
# echo: hello
```
