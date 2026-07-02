# ai-chat — UI + API + AI + agents on one riz binary

The flagship full-stack AI example: a React chat client, a Bun API running a
real **server-side agent loop** through riz's OpenAI-compatible gateway, and
the whole thing exposed to outside agents as a typed MCP tool. One binary, one
origin, no CORS, no second host.

```
browser ──► [static] React client
              │  POST /api/chat
              ▼
        [function.chat]  (Bun) ──► /_riz/v1/chat/completions  (the gateway)
              │   model answers with tool_calls                │
              │   handler executes lookup_order / get_time     ▼
              │   sends tool results back            mock │ anthropic │ openai │ ollama
              ▼
        final answer + tool trace ──► rendered in the UI

agents ──► /_riz/mcp  (the chat function is itself a typed MCP tool)
```

## Run it (offline, first try)

```bash
riz --dev          # from this directory; or headless: riz run
# open http://localhost:3000 and ask: "where is order 42?"
```

The default provider is **mock** — deterministic, no network, no API key — and
it exercises the *full* tool loop: the model calls `lookup_order`, the handler
executes it, the result goes back, the model answers. You can watch the tool
chips in the UI and the token panel in `riz --dev`.

## Point it at a real model

Edit `riz.toml`: uncomment a provider block (Anthropic / OpenAI / Ollama),
switch `default_provider`, set `CHAT_MODEL` in `[function.chat.env]` (e.g.
`anthropic/claude-sonnet-5`), and export the key named by `api_key_env`.
**The handler code does not change** — that's the gateway's job.

## What it demonstrates

| riz feature | where |
|---|---|
| Server-side agent loop via gateway tool-calling | `api/chat.ts` (`tools`, `tool_calls`, `role:"tool"`) |
| OpenAI-compatible gateway, provider by config | `[gateway]` in `riz.toml` |
| Per-function env vars | `[function.chat.env]` → `GATEWAY_URL`, `CHAT_MODEL` |
| UI + API on one origin | `[static]` serving `client/dist` |
| Your API as an agent tool | `claude mcp add riz --transport http http://localhost:3000/_riz/mcp` |
| Budget caps / usage telemetry | add `budget_usd` under `[gateway]`; `GET /_riz/v1/usage` |

## Client development

```bash
cd client && bun install
bun run dev        # Vite on :5173, proxying /api → riz on :3000
bun run build      # refresh the committed dist/ riz serves
```

`dist/` is committed intentionally so `riz run` works out of the box.

## Layout

- `riz.toml` — gateway + one Bun function (`POST /api/chat`) + static client
- `api/chat.ts` — tool definitions, the agent loop, the Lambda handler
- `client/` — Vite + React chat UI (tool-trace chips, token badge)
