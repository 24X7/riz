# events — JSON request body (Bun)

Accepts a POST JSON body, validates it, and echoes it back. Returns `400` when
the body is missing or not valid JSON.

**Capability:** request body handling + validation.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.events]
runtime = "bun"
handler = "./examples/lambdas/events/index.handler"
[[function.events.routes]]
path = "/events"
method = "ANY"
```

This same handler is also mounted behind the authorizer demo routes
`/protected` and `/forbidden` (see `auth-allow-bun/` and `auth-deny-bun/`).

## Run

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"event":"login","user":"alice"}' \
  http://127.0.0.1:3000/events
# {"received":{"event":"login","user":"alice"},"confirmedAt":...}

curl -X POST -d 'not json' http://127.0.0.1:3000/events
# 400 {"error":"body must be valid JSON"}
```
