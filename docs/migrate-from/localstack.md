# Migrating from LocalStack

You're probably here because Docker cold starts are killing your dev loop. LocalStack is excellent for full-AWS emulation but heavy if you only need the Lambda surface. Riz is the Lambda runtime without the rest.

## The 90-second decision

**Stay with LocalStack if** you need SQS, SNS, S3 events, DynamoDB, IAM, CloudFormation, or other AWS-service emulation in the same process.

**Switch to Riz if** your handlers only need HTTP API v2 + WebSocket and you'd like:
- Sub-second hot-reload instead of Docker container restarts
- A live terminal dashboard with P50–P99 latency
- A built-in MCP server
- The ability to run the same setup in production, not just local

**Run both** if you need LocalStack for cross-service tests but want Riz for daily dev. They don't conflict (Riz binds a port, LocalStack runs containers).

## What you'll move

| LocalStack thing | Riz equivalent |
|---|---|
| `docker run localstack/localstack` | `riz run` |
| `awslocal lambda create-function --code S3...` | Edit a file, hot-reload picks it up |
| `awslocal lambda invoke --function-name api` | `curl http://localhost:3000/api` or `riz mcp inspect` |
| `serverless.yml` Lambda + API GW block | `riz.toml` `[function.<name>]` block |
| LocalStack's `:4566` endpoint | Riz on `:3000` (configurable) |
| Cold start: seconds (Docker) | Cold start: ~50ms (process spawn) |
| Per-invocation overhead | Process pool stays warm |

## Mapping serverless.yml to riz.toml

A typical LocalStack-backed `serverless.yml`:

```yaml
service: my-api
provider:
  name: aws
  runtime: nodejs20.x
  region: us-east-1
functions:
  api:
    handler: src/api/index.handler
    timeout: 30
    events:
      - httpApi:
          path: /users/{id}
          method: GET
      - httpApi:
          path: /users
          method: POST
custom:
  serverless-offline:
    httpPort: 4566
```

The Riz equivalent:

```toml
# riz.toml
[server]
port = 3000
host = "127.0.0.1"

[function.api]
runtime = "bun"               # Bun runs node-compatible TS/JS at native speed
handler = "src/api/index.handler"
timeout_ms = 30000
concurrency = 10

[[function.api.routes]]
path = "/users/{id}"
method = "GET"

[[function.api.routes]]
path = "/users"
method = "POST"
```

That's it. The handler file at `src/api/index.ts` (or `index.js`) doesn't change.

## Running the migrated setup

```bash
# Stop LocalStack:
docker stop localstack

# Start Riz:
riz run
# (Headless mode is the default — TUI only on `riz --dev`)

# Verify routes:
riz routes
```

Hit the endpoints exactly as you would on AWS:

```bash
curl 'http://localhost:3000/users/42'
curl -X POST 'http://localhost:3000/users' -d '{"name":"alice"}'
```

## What changes about your dev loop

| Thing | LocalStack | Riz |
|---|---|---|
| Edit handler, see change | `awslocal lambda update-function-code` + Docker rebuild | Save the file, next request hits new code |
| Watch performance | Logs in a separate terminal | Live ratatui dashboard built in |
| Verify MCP setup | Write your own MCP wrapper | `riz mcp inspect` |
| Deploy to prod | Different infrastructure | Same binary, same `riz.toml` |

## What you give up

LocalStack emulates the rest of AWS. If your Lambda fires on S3 events, processes SQS messages, writes to DynamoDB, or talks to IAM — those are not in Riz's surface. v0.1 is HTTP API v2 + WebSocket only.

If you need both: keep LocalStack for the cross-service integration tests, use Riz for the inner dev loop.

## See also

- [docs/migrate-from/aws-lambda.md](./aws-lambda.md) — the canonical migration walkthrough
- [docs/migrate-from/sam-local.md](./sam-local.md) — if you came in through SAM
- [/vs](https://riz.dev/vs) — head-to-head feature comparison
