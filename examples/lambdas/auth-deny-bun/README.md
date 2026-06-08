# auth-deny-bun — REQUEST authorizer, deny path (Bun)

The rejecting counterpart to `auth-allow-bun`. Returns
`{ isAuthorized: false }`, which riz converts to a `401 Unauthorized` **without
ever invoking the protected handler**.

**Capability:** authorizer rejection short-circuits the request.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## How it's wired (`examples/riz.all.toml`)

```toml
[function.auth-deny]
runtime = "bun"
handler = "./examples/lambdas/auth-deny-bun/index.handler"

[function.forbidden]
runtime = "bun"
handler = "./examples/lambdas/events/index.handler"
authorizer = "auth-deny"           # ← always rejects
[[function.forbidden.routes]]
path = "/forbidden"
method = "ANY"
```

## Run

```bash
curl -w ' HTTP %{http_code}\n' http://127.0.0.1:3000/forbidden
# Unauthorized  HTTP 401
```

See `auth-allow-bun/` for the granting counterpart.
