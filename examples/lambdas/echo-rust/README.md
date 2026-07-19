# echo-rust — full event + context surface, runtime parity (Rust)

The Rust member of the echo parity set (`echo-bun`, `echo-python`,
`echo-rust`). Built on the official `lambda_runtime` crate, it compiles to a
static binary that riz drives through its per-worker AWS Lambda Runtime API.
`tests/runtime_parity_echo.rs` asserts its response matches the Bun and Python
echoes.

**Capability:** complete event surface + Lambda context using typed
`aws_lambda_events` structs. Honors `?status=NNN`.
**Runtime:** Rust · **Handler:** compiled binary `target/release/echo-rust`

## Build & wiring

```bash
cargo build --release --manifest-path examples/lambdas/echo-rust/Cargo.toml
# → target/release/echo-rust
```

```toml
[function.echo-rust]
runtime = "rust"
handler = "./target/release/echo-rust"
[[function.echo-rust.routes]]
path = "/echo-rust"
method = "ANY"
```

## Run

```bash
curl 'http://127.0.0.1:3000/echo-rust?name=alice'
# {"echo":"/echo-rust","method":"GET","functionName":"echo-rust",...}
```

## Footgun documented inline

`aws_lambda_events`' `QueryMap` deserializes the AWS v2 single-value shape
correctly but re-serializes to the multi-value shape (`{"name":["alice"]}`)
when round-tripped through `serde_json::Value`. The handler flattens it back to
single-value form so the body matches the other runtimes — see the comment in
`src/main.rs`. Every Rust Lambda that re-emits `queryStringParameters` hits
this.
