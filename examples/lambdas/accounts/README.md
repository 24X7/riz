# accounts — path parameters + query string (Bun)

Reads an AWS API Gateway v2 `{id}` path capture and parses the raw query
string, echoing both back in the response.

**Capability:** `pathParameters` + `rawQueryString` parsing.
**Runtime:** Bun · **Handler:** `index.ts` → `handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.accounts]
runtime = "bun"
handler = "./examples/lambdas/accounts/index.handler"
[[function.accounts.routes]]
path = "/accounts/{id}"
method = "GET"
```

## Run

```bash
curl 'http://127.0.0.1:3000/accounts/42?include=profile'
# {"id":"42","name":"Account 42","plan":"pro","include":"profile","ts":...}
```

The `{id}` segment arrives as `event.pathParameters.id`; `?include=` is pulled
from `event.rawQueryString`.
