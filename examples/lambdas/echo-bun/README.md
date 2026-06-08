# echo-bun — full event + context surface, runtime parity (Bun)

Echoes the **entire** AWS event envelope and Lambda context back as JSON. This
is the Bun member of a three-runtime parity set — `echo-bun`, `echo-python`,
and `echo-rust` emit the same response shape, which
`tests/runtime_parity_echo.rs` asserts byte-for-byte across runtimes.

**Capability:** complete event surface (`rawPath`, `method`, `body`,
`isBase64Encoded`, `pathParameters`, `queryStringParameters`, `stageVariables`,
`cookies`, `headers`, `authorizer`) + Lambda context (`functionName`,
`invokedFunctionArn`, `awsRequestId`, `getRemainingTimeInMillis()`). Sets a
response cookie and `x-riz-echo` header. Honors `?status=NNN` to override the
status code.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

The Bun handler additionally returns `invocationCount` (a per-process counter)
— this drives the cache-replay parity test: a cached hit replays the prior
response *including* its captured count, proving the handler wasn't re-run.
The Python and Rust echoes omit this Bun-only field.

## Wiring (`examples/riz.all.toml`)

```toml
[function.echo-bun]
runtime = "bun"
handler = "./examples/lambdas/echo-bun/index.handler"
[[function.echo-bun.routes]]
path = "/echo-bun"
method = "ANY"
```

## Run

```bash
curl 'http://127.0.0.1:3000/echo-bun?status=200&name=alice'
# {"echo":"/echo-bun","method":"GET","functionName":"echo-bun",...}

curl -s -o /dev/null -w '%{http_code}\n' 'http://127.0.0.1:3000/echo-bun?status=503'
# 503
```
