# rust-http riz template

A minimal AWS API Gateway v2 HTTP Lambda handler in Rust, ready to run
on riz (https://riz.dev).

## Build + run

```bash
cargo build --release
riz run
# → curl localhost:3000/hello?name=alice
#   {"message":"hello, alice","method":"GET", ...}
```

## Layout

- `Cargo.toml` — depends on `riz-rust-runtime` (the helper crate that
  handles the line-JSON wire protocol with the riz host)
- `src/main.rs` — the handler function plus `fn main() { run(handler); }`
- `riz.toml` — points at `./target/release/hello` (the built binary)

## Customizing

The handler signature is:

```rust
async fn handler(
    event: ApiGatewayV2httpRequest,
    ctx: Context,
) -> Result<ApiGatewayV2httpResponse, Box<dyn std::error::Error + Send + Sync>>
```

This is the exact AWS API Gateway v2 event/response shape, so handlers
written for real AWS Lambda compile here unchanged. Edit `src/main.rs`,
rebuild, and `riz run` picks up the new binary via handler-source
hot reload (no restart needed).
