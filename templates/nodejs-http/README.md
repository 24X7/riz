# nodejs-http riz template

A minimal AWS API Gateway v2 HTTP Lambda handler in plain ESM JavaScript,
ready to run on riz (https://riz.dev) via the system `node`. No build step,
no dependencies — Node loads `index.mjs` directly.

## Run

```bash
riz --dev          # or headless: riz run
# → curl "localhost:3000/hello?name=alice"
#   {"message":"hello, alice","method":"GET", ...}
```

Requires `node` on PATH. `riz doctor` checks this for you.

## Layout

- `index.mjs` — the handler: `export const handler = async (event, context) => ...`,
  the exact AWS Lambda shape. Handlers written for real AWS run here unchanged.
- `riz.toml` — `handler = "index.handler"` (AWS-style `file.export`), one
  `GET /hello` route.

## Customizing

- Edit `index.mjs`, save — hot reload means the next request hits the new
  code, no restart.
- Add routes: more `[[function.hello.routes]]` blocks in `riz.toml`.
  `{id}` and `{proxy+}` path params work exactly like AWS.
- Add functions: more `[function.<name>]` blocks, each with its own handler
  file and warm process pool.
- Serve a frontend on the same origin: point `[static]` at a build dir
  (see the `typescript-todo` template for a full React + API example).

## Your function is already an agent tool

The moment riz boots, every function is a typed MCP tool at `/_riz/mcp`:

```bash
claude mcp add riz --transport http http://localhost:3000/_riz/mcp
riz mcp inspect    # see the tool schema an agent sees
```
