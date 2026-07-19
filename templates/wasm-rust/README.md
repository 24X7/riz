# wasm-rust riz template

An AWS API Gateway v2 HTTP handler compiled to **`wasm32-wasip1`** and run on
riz (https://riz.dev) inside wasmtime's WASI capability sandbox —
deny-by-default: no filesystem, network, or host env unless you grant it.

Authored on **`riz-wasm`**: you write a Lambda handler, the shim owns the
event loop, the wire, and the capability ABI. Your whole program is
`fn handler(event, ctx)` plus `fn main() { riz_wasm::run(handler) }`.

## Build + run

```bash
rustup target add wasm32-wasip1          # once
cargo build --release --target wasm32-wasip1
riz --dev                                 # or headless: riz run
# → curl "localhost:3000/hello?name=alice"
#   {"message":"hello, alice","method":"GET","runtime":"wasm", ...}
```

## Layout

- `src/main.rs` — the handler (APIGW v2 event in, Lambda response out) on
  `riz_wasm::{Event, Context, Response}`; no event loop, no stdin, no unsafe.
- `Cargo.toml` — an independent workspace targeting `wasm32-wasip1`;
  dependencies are `serde_json` + `riz-wasm`.
- `riz.toml` — `runtime = "wasm"`, `handler` pointing at the built
  `./target/wasm32-wasip1/release/hello.wasm`, and a commented-out capability
  grant block showing how this function would reach Postgres.

## Reaching the outside world

The sandbox has no sockets — and doesn't need them. Declare a capability
grant in `riz.toml` (see the commented block) and call the typed client:

```rust
let rows = riz_wasm::cap::pg::query("db", "select 1 as one", &[])?;
```

The host performs the I/O on its own pools under the grant's limits
(deadlines, rate, in-flight, payload caps); credentials never enter the
guest. Errors are a closed set you can match on: `denied`, `throttled`,
`timeout`, `too_large`, `bad_request`, `backend`.

**WebSocket variant:** WS handlers live as a showcase in `examples/chat`;
scaffold any repo subdir with `riz new <owner>/<repo>/<subdir>`.

## Your function is already an agent tool

The moment riz boots, every function is a typed MCP tool at `/_riz/mcp`:

```bash
claude mcp add riz --transport http http://localhost:3000/_riz/mcp
riz mcp inspect    # see the tool schema an agent sees
```
