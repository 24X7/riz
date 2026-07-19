# typescript-node riz template

A minimal AWS API Gateway v2 HTTP Lambda handler in **TypeScript, run
directly by Node** (https://riz.dev, system `node`). No build step and no
bundler: Node's native type stripping executes `index.ts` as-is.

**Node floor: >= 22.18** (type stripping on by default). `riz new` prints a
warning at scaffold time if your `node` is older; `riz doctor` checks it too.

## Run

```bash
riz --dev          # or headless: riz run
# → curl "localhost:3000/hello?name=alice"
#   {"message":"hello, alice","method":"GET", ...}
```

## Layout

- `index.ts` — the handler: `export const handler = async (event, context) => ...`,
  typed with `@types/aws-lambda` — the exact AWS Lambda shape. Handlers
  written for real AWS run here unchanged.
- `riz.toml` — `handler = "./index.ts"` (explicit path: node's AWS-style
  auto-extension is `.mjs`, so a TypeScript entry names its file), one
  `GET /hello` route.
- `package.json` — dev-only `@types/aws-lambda` for the editor; nothing to
  install to run.

## Customizing

- Edit `index.ts`, save — hot reload means the next request hits the new
  code, no restart.
- Add routes: more `[[function.hello.routes]]` blocks in `riz.toml`.
- Add functions: more `[function.<name>]` blocks, each with its own handler
  file and warm process pool.

**WebSocket variant:** WS handlers ($connect/$disconnect/$default +
@connections push) live as a showcase in `examples/chat`; scaffold any repo
subdir with `riz new <owner>/<repo>/<subdir>`.

## Your function is already an agent tool

The moment riz boots, every function is a typed MCP tool at `/_riz/mcp`:

```bash
claude mcp add riz --transport http http://localhost:3000/_riz/mcp
riz mcp inspect    # see the tool schema an agent sees
```
