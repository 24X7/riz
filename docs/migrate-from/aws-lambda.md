# Migrating from AWS Lambda

Riz speaks the exact AWS API Gateway v2 wire format. **Your handler code does not change.** This guide walks the three concrete differences: where the handler lives, how routes are declared, and how you deploy.

## What you'll change

| | AWS Lambda | Riz |
|---|---|---|
| Handler code | TypeScript/Python/Rust function | Same function, unchanged |
| Event shape | `APIGatewayProxyEventV2` | `APIGatewayProxyEventV2` (literally the same — `aws_lambda_events` crate) |
| Context | `Context` with `getRemainingTimeInMillis`, ARNs, request IDs | Same shape, same fields |
| Route definition | Console / Terraform / CDK / SAM template | `riz.toml` |
| Deployment | `aws lambda update-function-code` + API GW redeploy | `riz deploy` (S3 hot-swap) or just save the file (hot-reload) |
| Cost | Per-invocation | Your VPS bill |
| MCP integration | Hand-write an MCP server that wraps the function | Auto-registered at `/_riz/mcp` the moment you `riz run` |

## Three handlers, three migrations

### TypeScript / JavaScript (Bun)

**On AWS Lambda** (a typical handler):

```typescript
// index.ts — runs on AWS Lambda
import type { APIGatewayProxyEventV2, Context } from "aws-lambda";

export const handler = async (
  event: APIGatewayProxyEventV2,
  context: Context
) => {
  const name = event.queryStringParameters?.name ?? "world";
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      message: `hello, ${name}`,
      requestId: context.awsRequestId,
      remainingMs: context.getRemainingTimeInMillis(),
    }),
  };
};
```

**On Riz** (same file, zero changes):

```typescript
// index.ts — runs on Riz, unchanged
import type { APIGatewayProxyEventV2, Context } from "aws-lambda";

export const handler = async (
  event: APIGatewayProxyEventV2,
  context: Context
) => {
  // ... same code, byte-for-byte
};
```

Add a `riz.toml`:

```toml
[server]
port = 3000
host = "127.0.0.1"

[function.api]
runtime = "bun"
handler = "./index.handler"   # AWS-style "file.export" — splits on the last dot
timeout_ms = 30000
concurrency = 10

[[function.api.routes]]
path = "/hello"
method = "GET"
```

```bash
riz run
# Now: curl 'http://localhost:3000/hello?name=alice'
```

### Python

**On AWS Lambda:**

```python
# main.py — runs on AWS Lambda
def lambda_handler(event, context):
    name = event.get("queryStringParameters", {}).get("name", "world")
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({
            "message": f"hello, {name}",
            "requestId": context.aws_request_id,
            "remainingMs": context.get_remaining_time_in_millis(),
        }),
    }
```

**On Riz** (same file). Add:

```toml
[function.api]
runtime = "python"
handler = "main.lambda_handler"
```

### Rust

The Rust runtime adapter spawns your **pre-built binary** and speaks line-delimited JSON over stdin/stdout. Use the `riz-rust-runtime` crate to wire it up:

```rust
// src/main.rs
use riz_rust_runtime::{run, ApiGatewayV2httpRequest, ApiGatewayV2httpResponse};

#[tokio::main]
async fn main() {
    run(handler).await
}

async fn handler(
    event: ApiGatewayV2httpRequest,
    ctx: riz_rust_runtime::Context,
) -> ApiGatewayV2httpResponse {
    let name = event
        .query_string_parameters
        .first("name")
        .unwrap_or("world".to_string());
    riz_rust_runtime::json(200, &serde_json::json!({
        "message": format!("hello, {name}"),
        "awsRequestId": ctx.aws_request_id,
        "remainingMs": ctx.get_remaining_time_in_millis(),
    }))
}
```

```toml
[function.api]
runtime = "rust"
handler = "./target/release/my-api"
```

```bash
cargo build --release
riz run
```

## What you don't need to change

- **Event field access** — `event.queryStringParameters`, `event.pathParameters`, `event.headers`, `event.body`, `event.requestContext.http.method`. All the same.
- **Context methods** — `getRemainingTimeInMillis()`, `awsRequestId`, `functionName`, `invokedFunctionArn`. All implemented.
- **Response shape** — `statusCode`, `headers`, `cookies`, `body`, `isBase64Encoded`. Verbatim.
- **Path parameters** — `{id}` single-segment captures, `{proxy+}` greedy captures. Identical AWS semantics.

## What Riz adds that AWS doesn't

- **Hot-reload.** Edit `index.ts`, save, the next request hits the new code. No `aws lambda update-function-code`.
- **Live terminal dashboard.** `riz run` opens a ratatui dashboard with P50–P99 latency, request logs, and cold-start counts in real time.
- **MCP server.** Every function in `riz.toml` is automatically an MCP tool at `/_riz/mcp`. Point Claude Code or Cursor at it; no wrapper code.
- **On-box safety.** Each handler runs in a process with `RLIMIT_CORE=0`, `RLIMIT_NOFILE=4096`, `RLIMIT_FSIZE=100MiB`, `PR_SET_NO_NEW_PRIVS`, and optionally a Landlock filesystem allowlist. The handler can't fill your disk or escalate privileges.

## What Riz doesn't have (yet)

- **Non-HTTP event sources.** SQS, SNS, S3 events, EventBridge — not in v0.1. Plan: v0.2.
- **Lambda Layers.** Vendor handler-local dependencies in the handler directory instead.
- **Custom domain mappings.** Terminate Let's Encrypt at a reverse proxy in front of Riz.

If your Lambda only handles HTTP + WebSocket, none of these matter.

## Deployment

### Single-host

The 30-second deploy is just:

```bash
scp riz target-host:/usr/local/bin/
scp riz.toml index.ts target-host:/srv/myapp/
ssh target-host 'cd /srv/myapp && riz run --no-tui'
```

Or wrap in a systemd unit. The `docs/production.md` doc has an example.

### Hot-swap from S3

For zero-downtime updates without restarting Riz:

```bash
zip -r myhandler.zip index.ts node_modules/
aws s3 cp myhandler.zip s3://my-bucket/myhandler.zip
curl -X POST http://target-host:3000/_riz/deploy \
  -H 'authorization: Bearer $TOKEN' \
  -d '{"lambda":"api","s3_bucket":"my-bucket","s3_key":"myhandler.zip"}'
```

Riz downloads, unpacks to a staging dir, hot-swaps the process pool, drains in-flight requests, and rolls back on health-check failure.

## Verifying the migration

```bash
riz doctor       # validates riz.toml + checks runtime binaries
riz routes       # prints the route table
riz run          # boot the runtime
riz mcp inspect  # in another terminal: verifies MCP exposure
```

If `riz mcp inspect` lists your function, Claude Code can call it.

## See also

- [docs/mcp/getting-started.md](../mcp/getting-started.md) — point Claude / Cursor at the new endpoint
- [docs/migrate-from/sam-local.md](./sam-local.md) — if you came in through SAM
- [docs/migrate-from/localstack.md](./localstack.md) — if you came in through LocalStack
