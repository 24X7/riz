# wasm-http riz template

A minimal AWS API Gateway v2 HTTP handler compiled to **`wasm32-wasip1`** and
run on riz (https://riz.dev) inside wasmtime's WASI capability sandbox.

No riz library and no external runtime: riz embeds wasmtime and speaks a simple
line-delimited JSON envelope to the module over stdio. The `.wasm` you build
here is the artifact riz runs — deny-by-default (stdio only; no filesystem,
network, or host env unless you grant it).

## Build + run

```bash
rustup target add wasm32-wasip1          # once
cargo build --release --target wasm32-wasip1
riz --dev                                 # or headless: riz run
# → curl "localhost:3000/hello?name=alice"
#   {"message":"hello, alice","method":"GET","runtime":"wasm", ...}
```

## Layout

- `Cargo.toml` — an independent workspace targeting `wasm32-wasip1`; the only
  dependency is `serde_json` (pure sync std — no tokio, no networking)
- `src/main.rs` — a stdin loop that reads the request envelope and writes a
  gateway-shaped response
- `riz.toml` — `runtime = "wasm"`, `handler` pointing at the built
  `./target/wasm32-wasip1/release/hello.wasm`

## The contract

Each request arrives as one JSON line on stdin:

```json
{ "event": { ...API Gateway v2 event... }, "__riz_deadline_ms": 0, "__riz_function_name": "hello" }
```

The module writes one gateway-shaped response line to stdout:

```json
{ "statusCode": 200, "headers": {...}, "body": "...", "isBase64Encoded": false, "cookies": [] }
```

Edit `src/main.rs`, rebuild for the wasm target, and riz picks up the new
module via handler-source hot reload (no restart needed).

## The sandbox

WASM functions run deny-by-default. To let a handler do more, add to `riz.toml`:

- `allowed_paths = ["/data"]` — WASI filesystem preopens
- `[function.hello.stage_variables]` — guest environment variables
- `[function.hello.capabilities.<name>]` — brokered resources (e.g. Postgres)

See `examples/riz.all.toml` for every field.

## Next steps

- Add routes: more `[[function.hello.routes]]` blocks in `riz.toml`
  (`{id}` and `{proxy+}` path params work exactly like AWS).
- Your function is already a typed MCP tool at `/_riz/mcp`:
  `claude mcp add riz --transport http http://localhost:3000/_riz/mcp`
- Pre-flight check: `riz doctor`
