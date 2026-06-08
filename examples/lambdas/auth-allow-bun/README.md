# auth-allow-bun — REQUEST authorizer, allow path (Bun)

An AWS API Gateway v2 HTTP API **REQUEST authorizer** that grants access. It
returns the simple-response format and attaches context that the downstream
handler can read.

**Capability:** `authorizer = "<fn>"` wiring; simple-response authorizer
(`{ isAuthorized, context }`); context injection.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## How it's wired (`examples/riz.all.toml`)

The authorizer is registered as its own function, then referenced from the
protected function:

```toml
[function.auth-allow]
runtime = "bun"
handler = "./examples/lambdas/auth-allow-bun/index.handler"

[function.protected]
runtime = "bun"
handler = "./examples/lambdas/events/index.handler"
authorizer = "auth-allow"          # ← call auth-allow before the handler
[[function.protected.routes]]
path = "/protected"
method = "ANY"
```

The `context` it returns (`principalId`, `tier`) surfaces on the downstream
event as `requestContext.authorizer.fields.*`.

## Run

```bash
curl -w ' HTTP %{http_code}\n' -X POST \
  -H 'content-type: application/json' -d '{"event":"hello"}' \
  http://127.0.0.1:3000/protected
# {"received":{"event":"hello"},...}  HTTP 200
```

See `auth-deny-bun/` for the rejecting counterpart.
