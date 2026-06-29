# typescript-todo — a full app on one riz binary

A TodoMVC-style app whose **API and client ship on the same binary and the same
origin**. The React client (`client/`) is built with Vite; riz serves the build
output via a `[static]` block while a Bun function (`api/todos.ts`) answers
`/api/todos`. Because they share an origin, the client calls the API with a
plain `fetch("/api/todos")` — **no CORS, no second host, no extra infra.**

```
typescript-todo/
├── riz.toml            # the todos function (4 routes) + [static] → client/dist
├── api/
│   └── todos.ts        # Bun handler: in-memory TodoMVC CRUD
└── client/             # Vite + React TodoMVC client
    ├── src/            # App.tsx, api.ts, …
    └── dist/           # built output, served by riz (committed so it runs OOB)
```

## Run it

The committed `client/dist` lets it run immediately:

```bash
cd examples/typescript-todo
riz run                       # http://localhost:3000
```

Open <http://localhost:3000> — the React app is served by riz and talks to the
function on the same origin.

## Rebuild the client

```bash
cd client
bun install        # or: npm install
bun run build      # or: npm run build   → writes client/dist
```

Then `riz run` from the example root picks up the new build. For a live dev loop
with hot reload, run `riz run` in one terminal and `bun run dev` in another —
Vite's dev server (`:5173`) proxies `/api/*` to riz (`:3000`), so the same fetch
calls work in dev and in the colocated production build.

## What this demonstrates

- **Colocation** — one `riz run` serves both the API and the SPA. The same
  pattern deploys as a single binary behind one origin.
- **The AWS shape** — one `[function.todos]` pool serves four routes
  (`GET`/`POST /api/todos`, `PATCH`/`DELETE /api/todos/{id}`), exactly as API
  Gateway v2 would.
- **Precedence** — `/api/*` is owned by the function, so the SPA fallback never
  shadows it; everything else falls through to the static client.
- **Agent-ready for free** — the same function is also an MCP tool at
  `/_riz/mcp`, and `riz scaffold static` can generate `llms.txt` /
  `.well-known/riz.json` from this config so a live instance describes itself.
