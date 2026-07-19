# rust riz template

A minimal AWS API Gateway v2 HTTP Lambda handler in Rust, ready to run
on riz (https://riz.dev).

It uses the **official AWS Lambda Rust runtime** (`lambda_runtime`) — no riz
library. riz speaks the real AWS Lambda Runtime API to your binary, so this
exact executable runs unmodified on AWS Lambda and on riz.

## Build + run

```bash
cargo build --release
riz --dev          # or headless: riz run
# → curl "localhost:3000/hello?name=alice"
#   {"message":"hello, alice","method":"GET", ...}
```

## Layout

- `Cargo.toml` — depends on `lambda_runtime` + `aws_lambda_events` (the
  same crates you'd use on real AWS Lambda)
- `src/main.rs` — the handler plus `run(service_fn(handler))`
- `riz.toml` — points at `./target/release/hello` (the built binary)

## Customizing

The handler signature is the official AWS one:

```rust
async fn handler(
    event: LambdaEvent<ApiGatewayV2httpRequest>,
) -> Result<ApiGatewayV2httpResponse, Error>
```

`event.payload` is the API Gateway v2 event, `event.context` the Lambda
context (request id, deadline, function name). Handlers written for real
AWS Lambda compile here unchanged. Edit `src/main.rs`, rebuild, and riz
picks up the new binary via handler-source hot reload (no restart needed).

## Next steps

- Add routes: more `[[function.hello.routes]]` blocks in `riz.toml`
  (`{id}` and `{proxy+}` path params work exactly like AWS).
- Your function is already a typed MCP tool at `/_riz/mcp`:
  `claude mcp add riz --transport http http://localhost:3000/_riz/mcp`
- Pre-flight check: `riz doctor`

**WebSocket variant:** WS handlers ($connect/$disconnect/$default +
@connections push) live as a showcase in `examples/chat`; scaffold any repo
subdir with `riz new <owner>/<repo>/<subdir>`.
