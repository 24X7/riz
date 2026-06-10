#!/usr/bin/env python3
"""riz × Claude Agent SDK — the agent-substrate flagship demo.

riz's pitch: every riz function auto-becomes an MCP tool at /_riz/mcp the
moment `riz run` boots. This script proves the payoff — it points the
*Claude Agent SDK* at that endpoint (Streamable HTTP) and lets Claude
accomplish a real task by calling your riz functions as tools, with NO
glue code: the tools are just your handlers.

Task we give Claude:
  "Look up order 1042. If it's delayed, open a support ticket for it."

Behind the scenes Claude will:
  1. call  mcp__riz__lookup_order   (GET /orders/{id})   → sees delayed=true
  2. call  mcp__riz__create_ticket  (POST /tickets)      → opens the ticket
  3. summarize what it did.

Every token Claude spends here is attributable through riz's Phase-2
token-attribution spans (OTLP gen-ai attributes) when the same box also
fronts the model via riz's LLM gateway — the agent loop and the cost
ledger live in one place.

────────────────────────────────────────────────────────────────────────
API surface (confirmed from the official docs, June 2026):
  Doc: https://code.claude.com/docs/en/agent-sdk/mcp
       https://code.claude.com/docs/en/agent-sdk/python
  pip install claude-agent-sdk

  from claude_agent_sdk import query, ClaudeAgentOptions, ...
  options = ClaudeAgentOptions(
      model="claude-opus-4-8",
      mcp_servers={"riz": {"type": "http", "url": ".../_riz/mcp"}},
      allowed_tools=["mcp__riz__*"],          # mcp__<server>__<tool>
      thinking={"type": "adaptive", "display": "summarized"},
      include_partial_messages=True,          # streaming partials
  )
  async for message in query(prompt=..., options=options): ...

  - Remote Streamable HTTP MCP server: {"type": "http", "url": ...}.
    ("streamable-http" is an accepted alias in JSON config; the
     programmatic mcp_servers option takes "http".)
  - MCP tools are named  mcp__<server-name>__<tool-name>  and must be
    granted via allowed_tools (wildcard ok) or Claude won't call them.
────────────────────────────────────────────────────────────────────────

Prerequisites:
  pip install claude-agent-sdk
  export ANTHROPIC_API_KEY=sk-ant-...
  # In another terminal, boot riz with the agent-tools config:
  riz --config examples/riz.agent.toml run

Run:
  python3 examples/agent-sdk/agent_demo.py
  # optional override:
  RIZ_MCP_URL=http://localhost:3000/_riz/mcp python3 examples/agent-sdk/agent_demo.py

NOT run in CI — it needs a real API key. The substrate it drives
(tools/list + tools/call over /_riz/mcp) IS proven deterministically and
without a key by tests/examples_agent.rs.
"""

from __future__ import annotations

import asyncio
import os
import sys

# riz's MCP endpoint. Streamable HTTP, JSON-RPC 2.0, spec 2025-11-25.
RIZ_MCP_URL = os.environ.get("RIZ_MCP_URL", "http://localhost:3000/_riz/mcp")

# House defaults: flagship model, adaptive thinking, streaming.
MODEL = os.environ.get("RIZ_AGENT_MODEL", "claude-opus-4-8")

TASK = (
    "Look up the status of order 1042 using the available tools. "
    "If the order is delayed, open a support ticket for it explaining the "
    "delay (pass the order id and a short reason). Then tell me, in one or "
    "two sentences, exactly what you found and what you did."
)


async def main() -> int:
    try:
        from claude_agent_sdk import (
            query,
            ClaudeAgentOptions,
            AssistantMessage,
            SystemMessage,
            ResultMessage,
        )
    except ImportError:
        print(
            "claude-agent-sdk is not installed.\n"
            "  pip install claude-agent-sdk",
            file=sys.stderr,
        )
        return 2

    if not os.environ.get("ANTHROPIC_API_KEY"):
        print(
            "ANTHROPIC_API_KEY is not set — this demo calls the real model.\n"
            "  export ANTHROPIC_API_KEY=sk-ant-...\n"
            "(The riz MCP substrate itself is proven without a key by "
            "tests/examples_agent.rs.)",
            file=sys.stderr,
        )
        return 2

    options = ClaudeAgentOptions(
        model=MODEL,
        # Connect Claude to riz's MCP endpoint over Streamable HTTP. Server
        # name "riz" → tools are addressed as mcp__riz__<tool>.
        mcp_servers={
            "riz": {
                "type": "http",
                "url": RIZ_MCP_URL,
            }
        },
        # Auto-approve every riz tool so Claude can call them without a prompt.
        allowed_tools=["mcp__riz__*"],
        # House style: adaptive thinking, summarized; streaming partials.
        thinking={"type": "adaptive", "display": "summarized"},
        include_partial_messages=True,
    )

    print(f"→ riz MCP endpoint : {RIZ_MCP_URL}")
    print(f"→ model            : {MODEL}")
    print(f"→ task             : {TASK}\n")

    async for message in query(prompt=TASK, options=options):
        # Confirm the MCP server connected and show which tools riz exposed.
        if isinstance(message, SystemMessage) and message.subtype == "init":
            servers = message.data.get("mcp_servers", [])
            for s in servers:
                print(f"[mcp] server {s.get('name')!r}: {s.get('status')}")

        # Narrate each tool call Claude makes against riz.
        if isinstance(message, AssistantMessage):
            for block in message.content:
                name = getattr(block, "name", None)
                if name and name.startswith("mcp__"):
                    print(f"[tool] Claude called {name}  args={getattr(block, 'input', {})}")
                text = getattr(block, "text", None)
                if text:
                    print(f"[claude] {text}")

        # Final result.
        if isinstance(message, ResultMessage):
            if message.subtype == "success":
                print(f"\n=== result ===\n{message.result}")
            else:
                print(f"\n[agent ended: {message.subtype}]", file=sys.stderr)
                return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
