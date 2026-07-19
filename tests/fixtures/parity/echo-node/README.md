# echo-node — full event + context surface, runtime parity (Node.js)

The Node.js member of the echo parity set (`echo-bun`, `echo-python`,
`echo-rust`, `echo-node`). Echoes the AWS event envelope and Lambda context
back as JSON in the same shape as the other runtimes —
`tests/runtime_parity_echo.rs` asserts they match.

**Capability:** complete event surface + Lambda context, from a standard
`export const handler = async (event, context) => …`. Honors `?status=NNN`.
**Runtime:** Node.js · **Handler:** `index.mjs` → `handler`

Node is the #1 production AWS Lambda runtime, so existing Node Lambda code drops
in unmodified — same `index.handler` shape, no SDK, no build step (plain ESM).

## Wiring (`examples/riz.all.toml`)

```toml
[function.echo-node]
runtime = "node"
handler = "./tests/fixtures/parity/echo-node/index.handler"
[[function.echo-node.routes]]
path = "/echo-node"
method = "ANY"
```

## Run

```bash
curl 'http://127.0.0.1:3000/echo-node?status=200&name=alice'
# {"echo":"/echo-node","method":"GET","functionName":"echo-node",...}
```

Like `echo-bun`, this handler returns an `invocationCount` (per-process counter)
used by the cache-replay test; `echo-rust` omits it.
