# Claude Agent SDK over riz MCP — the agent-substrate flagship

This is the demo that proves riz's core pitch: **every riz function
auto-becomes an MCP tool**, so an agent can drive your APIs with zero glue
code. Here the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/python)
connects to riz's MCP endpoint and lets Claude accomplish a real task by
calling your riz functions as tools.

## What it demonstrates

`riz --config examples/riz.agent.toml run` boots three ordinary handlers
(`examples/lambdas/agent-tools/index.ts`). The moment it boots, each one is
a typed MCP tool at `http://localhost:3000/_riz/mcp`:

| riz function     | route               | MCP tool (`mcp__riz__…`) |
| ---------------- | ------------------- | ------------------------- |
| `lookup_order`   | `GET  /orders/{id}` | `mcp__riz__lookup_order`  |
| `list_inventory` | `GET  /inventory`   | `mcp__riz__list_inventory`|
| `create_ticket`  | `POST /tickets`     | `mcp__riz__create_ticket` |

`agent_demo.py` points the Claude Agent SDK at that endpoint and asks:

> "Look up order 1042. If it's delayed, open a support ticket for it."

Claude discovers the tools via MCP `tools/list`, calls `lookup_order`
(sees `delayed: true`), then calls `create_ticket` — all driven by the SDK
over the same `tools/call` path that `tests/examples_agent.rs` proves
deterministically. No SDK code on the riz side; the tools are just your
handlers.

Because riz also fronts the model through its OpenAI-compatible gateway,
the tokens Claude spends are attributable through riz's Phase-2
token-attribution spans (OTLP gen-ai attributes): the agent loop and the
cost ledger live on one box.

## Prerequisites

```bash
pip install claude-agent-sdk
export ANTHROPIC_API_KEY=sk-ant-...

# In another terminal, from the repo root, boot riz with the agent config:
riz --config examples/riz.agent.toml run
```

## Run

```bash
python3 examples/agent-sdk/agent_demo.py
```

Override the endpoint or model if needed:

```bash
RIZ_MCP_URL=http://localhost:3000/_riz/mcp \
RIZ_AGENT_MODEL=claude-opus-4-8 \
  python3 examples/agent-sdk/agent_demo.py
```

Defaults (house style): model `claude-opus-4-8`, adaptive thinking,
streaming.

## The API surface (confirmed from the docs)

Confirmed June 2026 from
<https://code.claude.com/docs/en/agent-sdk/mcp> and
<https://code.claude.com/docs/en/agent-sdk/python>:

```python
from claude_agent_sdk import query, ClaudeAgentOptions

options = ClaudeAgentOptions(
    model="claude-opus-4-8",
    mcp_servers={"riz": {"type": "http", "url": ".../_riz/mcp"}},  # Streamable HTTP
    allowed_tools=["mcp__riz__*"],                                  # mcp__<server>__<tool>
    thinking={"type": "adaptive", "display": "summarized"},
    include_partial_messages=True,
)
async for message in query(prompt="…", options=options):
    ...
```

- Remote Streamable HTTP MCP server config: `{"type": "http", "url": …}`.
- MCP tools are named `mcp__<server-name>__<tool-name>` and must be granted
  via `allowed_tools` (a wildcard like `mcp__riz__*` is fine) or Claude
  won't be allowed to call them.

## Not run in CI

This script needs a real `ANTHROPIC_API_KEY`, so it is **not** part of the
test suite. The substrate it exercises is proven without a key or network
by [`tests/examples_agent.rs`](../../tests/examples_agent.rs): that test
boots riz with `examples/riz.agent.toml` and asserts, over `/_riz/mcp`,
that `tools/list` exposes these functions as named MCP tools with input
schemas and that a `tools/call` returns the expected structured result —
the exact path the Agent SDK drives.
