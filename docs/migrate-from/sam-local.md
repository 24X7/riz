# Migrating from SAM Local

You're probably here because `sam local start-api` spawns a fresh Docker container per invocation and that's a slow dev loop. Riz reads a flat `riz.toml` instead of a SAM template and keeps a warm process pool — same Lambda code, sub-second hot-reload.

## When SAM Local still wins

- You're already deep in CloudFormation / SAM templates and your team's deploy pipeline depends on them.
- You need Lambda Layers (Riz doesn't ship Layers — vendor your deps in the handler dir instead).
- You need to invoke Lambdas from the same SAM CLI used in CodeBuild/CodePipeline.

If none of those apply, Riz is the faster dev loop.

## SAM template → riz.toml

A common SAM template:

```yaml
# template.yaml
AWSTemplateFormatVersion: '2010-09-09'
Transform: AWS::Serverless-2016-10-31
Resources:
  ApiFunction:
    Type: AWS::Serverless::Function
    Properties:
      Runtime: nodejs20.x
      CodeUri: src/api/
      Handler: index.handler
      Timeout: 30
      Events:
        GetUser:
          Type: HttpApi
          Properties:
            Path: /users/{id}
            Method: GET
        PostUser:
          Type: HttpApi
          Properties:
            Path: /users
            Method: POST
```

Riz equivalent:

```toml
# riz.toml
[server]
port = 3000
host = "127.0.0.1"

[function.api]
runtime = "bun"
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

The handler file (`src/api/index.ts`) is unchanged.

## Mapping common SAM idioms

| SAM | Riz |
|---|---|
| `sam local start-api --port 3001` | `riz run` (port in `[server]` block) |
| `sam local invoke ApiFunction -e event.json` | `curl ... http://localhost:3000/...` (Riz speaks HTTP, not direct invoke) |
| `sam build` then `sam local start-api` | `riz run` (no build step for Bun/Python; Rust needs `cargo build --release`) |
| `Environment.Variables` | `[function.<name>.environment]` (set process env at spawn time) |
| `Events.MyEvent.Type: HttpApi` | `[[function.<name>.routes]]` block |
| `{proxy+}` greedy | `path = "/{proxy+}"` (identical syntax) |
| `Globals.Api.Cors` | `[cors]` block — global or per-function |

## What about authorizers?

SAM's `Authorizers.RequestAuthorizer` maps to Riz's `[function.<name>.authorizer]` block:

```toml
[function.api.authorizer]
type = "request"
function = "auth"   # name of another function in this riz.toml
ttl_secs = 300      # cache positive results (same semantic as IAM identitySource caching)

[function.auth]
runtime = "bun"
handler = "src/auth/index.handler"
timeout_ms = 5000
concurrency = 5
```

JWT authorizers map similarly with `type = "jwt"` and `jwks_uri = "..."`.

## What you don't have to change

Your **handler code does not change.** Same `APIGatewayProxyEventV2`, same `Context`, same path-parameter shape, same response envelope. Riz uses the official `aws_lambda_events` crate types verbatim.

## What's missing vs SAM Local

- **Lambda Layers** — not in v0.1. Vendor your dependencies in the handler dir.
- **Lambda Extensions** — same.
- **Direct invocation (`sam local invoke`)** — Riz is HTTP-native. Use `curl` or `riz mcp inspect`.
- **CloudFormation parameter handling** — `riz.toml` is flat config, not a template language.

## After you switch

```bash
riz doctor       # validates the new riz.toml + checks bun/python3 are on PATH
riz routes       # prints the route table the way SAM would
riz run          # boot the runtime
```

Open a second terminal and hit the routes as before. The first cold start is a process spawn (~50ms), not a Docker container build (seconds).

## See also

- [docs/migrate-from/aws-lambda.md](./aws-lambda.md) — the canonical migration walkthrough
- [docs/migrate-from/localstack.md](./localstack.md) — if you came in through LocalStack
- [/vs](https://riz.dev/vs) — head-to-head feature comparison
