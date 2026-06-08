# ping — health-check handler (Bun)

The smallest possible riz handler: no input, a fixed JSON response. Use it as
the "hello world" when wiring up a new config or checking the server is live.

**Capability:** minimal HTTP handler, no path/query/body.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.ping]
runtime = "bun"
handler = "./examples/lambdas/ping/index.handler"
# No routes block → implicit default is `ANY /ping`.
```

## Run

```bash
curl http://127.0.0.1:3000/ping
# {"status":"ok","ts":1733600000000}
```
