# A2A: riz as a built-in agent (server + client mesh)

**Date:** 2026-07-02 · **Status:** approved (built-in agent; server + client)

## Why

MCP is southbound (agents consume riz functions as tools). A2A
(a2a-protocol.org, v1.0.0, Linux Foundation) is northbound: riz **is** an
agent that peers discover, delegate tasks to, and stream results from. One
binary = tools (MCP) + model plane (gateway) + delegable worker (A2A).
The deterministic mock provider means a full A2A task — delegate → reason →
call tools → answer — runs offline in CI with zero keys.

## Config

```toml
[agent]                          # opt-in; absent = no A2A surface
name = "shop-support"            # Agent Card identity
description = "Answers order questions using the shop's own functions"
model = "mock"                   # any gateway model ("anthropic/claude-…")
system_prompt = "You are …"
tools = ["orders", "inventory"]  # allowlist; default = all HTTP functions
max_hops = 5                     # agent-loop cap (same as ai-chat)
task_timeout_ms = 60000

[agent.peers]                    # A2A client side (PR 3)
warehouse = "https://wh.internal:3000"
```

`[agent]` requires `[gateway]` (validation error otherwise). Bearer gating
follows the standard `/_riz/*` rule and is declared in the Agent Card's
`securitySchemes` (`http` bearer).

## Server (PRs 1–2 of this track)

- **Agent Card** at `/.well-known/agent-card.json` — generated LIVE from
  config + function state (like the MCP resources): identity, service
  endpoint `/_riz/a2a`, `capabilities.streaming`, skills derived from the
  tool allowlist, securitySchemes, `A2A-Version` support.
- **JSON-RPC binding** at `POST /_riz/a2a` (reuses the MCP layer's JSON-RPC
  plumbing): `SendMessage`, `GetTask`, `CancelTask` first;
  `SendStreamingMessage` (SSE) in the follow-up PR; push-notification configs
  deferred.
- **Task store**: in-memory, the 8-state lifecycle (`SUBMITTED → WORKING →
  COMPLETED/FAILED/CANCELED/REJECTED`, `INPUT_REQUIRED`/`AUTH_REQUIRED`
  reserved), task history + artifacts. Tasks run on a spawned tokio task;
  `CancelTask` flips state and aborts best-effort.
- **The brain**: the ai-chat agent loop promoted into the runtime — messages
  → gateway chat with the allowlisted functions as OpenAI tools → execute
  `tool_calls` by dispatching through the Router (same synthetic-event path
  as MCP `tools/call`, including WS ephemeral sessions once merged) → tool
  results back to the model → final text becomes the task's artifact. Token
  usage rolls into the FinOps ledger + OTel GenAI spans like every gateway
  call; `budget_usd` caps the agent too.

## Client / mesh (PR 3 of this track)

Each `[agent.peers]` entry: fetch + cache the peer's Agent Card at boot
(refresh on TTL), expose the peer to the built-in agent as a tool
`delegate_to_<name>` (schema `{ message: string }`) that issues
`SendMessage` and returns the completed task's artifacts. Loop protection: a
`riz-a2a-hop` extension header, rejected past `max_hops`. `riz a2a send
<url> <message>` CLI for manual testing. That is the riz-to-riz mesh: any
instance can delegate to any other; each side meters its own tokens.

## PR breakdown

1. **a2a-server-core** — config, Agent Card, task store, `SendMessage` /
   `GetTask` / `CancelTask`, runtime agent loop, e2e offline via mock.
2. **a2a-streaming** — `SendStreamingMessage` over SSE (status + artifact
   deltas), riding the gateway streaming machinery.
3. **a2a-client-mesh** — peers as `delegate_to_*` tools, hop caps, CLI, e2e:
   two in-process riz instances, one delegates to the other, all offline.

Each PR ships its own website updates (agents.html "riz IS an agent"
section, docs.html `[agent]` reference, llms.txt / riz.json, CAPABILITY-CARD)
and pins its e2e proof tests in `tests/claims/registry.toml`.

## Testing

All offline via the mock provider: SendMessage completes a task whose
artifact embeds a tool result; task lifecycle transitions observable via
GetTask; cancel; card validity (schema-shaped, live function skills); bearer
gating; budget cap → task FAILED with a clear error; mesh e2e
(two instances); hop-cap rejection.

## Out of scope (v1)

gRPC + REST bindings (JSON-RPC only; the card advertises accordingly),
push notifications, `INPUT_REQUIRED` interactive tasks, OAuth/OIDC/mTLS
schemes (bearer only), persistent task store.
