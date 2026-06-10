#!/usr/bin/env python3
"""
agent-loop — a multi-step agent that LOOPS over riz MCP tools.

Where examples/agent-sdk/agent_demo.py runs a single conditional task, this one
drives a *batch*: for each order id, the agent calls `lookup_order`, and for any
order that comes back delayed it calls `create_ticket`. That's many tool calls
chained under one task — the shape that makes per-request token attribution
actually interesting (see TOKEN ATTRIBUTION in README.md).

Every riz function is already an MCP tool (lookup_order / list_inventory /
create_ticket — see examples/riz.agent.toml). No SDK glue on the riz side.

Prereqs (NOT run in CI — needs a real key + a running riz):
    pip install claude-agent-sdk
    export ANTHROPIC_API_KEY=sk-ant-...
    riz --config examples/riz.agent.toml run        # serves /_riz/mcp on :3000
    python3 examples/agent-loop/agent_loop.py

API surface confirmed at https://code.claude.com/docs/en/agent-sdk/python
(query, ClaudeAgentOptions, mcp_servers http transport, allowed_tools).
"""
import anyio

from claude_agent_sdk import (
    query,
    ClaudeAgentOptions,
    AssistantMessage,
    ResultMessage,
)

RIZ_MCP_URL = "http://localhost:3000/_riz/mcp"

# Seeded in examples/lambdas/agent-tools/index.ts — order 1042 is delayed.
ORDER_IDS = [1041, 1042, 1043]


async def main() -> None:
    options = ClaudeAgentOptions(
        model="claude-opus-4-8",  # house default: flagship model
        mcp_servers={"riz": {"type": "http", "url": RIZ_MCP_URL}},
        allowed_tools=["mcp__riz__*"],  # grant the riz tools (mcp__<server>__<tool>)
        thinking={"type": "adaptive", "display": "summarized"},
        include_partial_messages=True,  # stream
    )

    prompt = (
        "For each of these order ids, call lookup_order to get its status: "
        f"{', '.join(map(str, ORDER_IDS))}. "
        "For every order that is delayed, call create_ticket with a short summary "
        "naming the order id. When you're done, give me a one-line report of which "
        "orders were ticketed."
    )

    async for message in query(prompt=prompt, options=options):
        if isinstance(message, AssistantMessage):
            for block in message.content:
                text = getattr(block, "text", None)
                if text:
                    print(text, end="", flush=True)
        elif isinstance(message, ResultMessage):
            # ResultMessage carries the SDK's own usage/cost summary; riz also
            # attributes the same tokens per-request in its OTLP spans + --dev TUI.
            print("\n\n[done]", getattr(message, "usage", ""))


if __name__ == "__main__":
    anyio.run(main)
