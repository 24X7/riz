# crud-accounts — full CRUD across every HTTP verb (Bun)

A single handler that switches on the HTTP method to implement
GET / POST / PUT / PATCH / DELETE against an in-memory `/accounts` resource.

**Capability:** method dispatch, all five verbs, per-process state.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.crud-accounts]
runtime = "bun"
handler = "./examples/lambdas/crud-accounts/index.handler"
[[function.crud-accounts.routes]]
path = "/accounts"      # POST creates
method = "ANY"
[[function.crud-accounts.routes]]
path = "/crud/{id}"     # GET / PUT / PATCH / DELETE by id
method = "ANY"
```

> The `{id}` routes use `/crud/{id}` here (not `/accounts/{id}`) to avoid
> colliding with the read-only `accounts` function in the same all-in-one
> config.

## Run

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"name":"alice","plan":"pro"}' http://127.0.0.1:3000/accounts   # 201
curl http://127.0.0.1:3000/crud/1                                     # 200
curl -X DELETE http://127.0.0.1:3000/crud/1                           # 204
```

**State note:** the store is a module-level `Map`, so it lives only inside one
worker process and resets when that process is replaced. With
`concurrency > 1` a read can land on a different worker than the write — this
is a local-dev illustration of verb dispatch, not a durable datastore.
