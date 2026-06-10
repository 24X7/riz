# agent-loop — multi-step tool chain + token attribution

A Claude Agent SDK script that **loops** over riz MCP tools: for a batch of order
ids it calls `lookup_order` on each, then `create_ticket` for any that are delayed.
One task, many chained tool calls — the case where "what did this request cost in
tokens?" stops being trivial.

This complements [`../agent-sdk/`](../agent-sdk/) (a single conditional task) by
showing the **batch-loop** pattern.

## Run it (needs a real key + a running riz — not run in CI)

```bash
pip install claude-agent-sdk
export ANTHROPIC_API_KEY=sk-ant-...
riz --config examples/riz.agent.toml run        # serves /_riz/mcp on :3000
python3 examples/agent-loop/agent_loop.py
```

`examples/riz.agent.toml` exposes three functions that auto-become MCP tools —
`lookup_order`, `list_inventory`, `create_ticket` (order `1042` is seeded delayed).
No SDK glue on the riz side: every function is already a tool.

## Token attribution (why the loop matters)

riz emits **OTel GenAI** spans on the one OTLP/HTTP path (see
[`docs/superpowers/specs/2026-06-10-observability-design.md`](../../docs/superpowers/specs/2026-06-10-observability-design.md)):
an inbound request opens a root span, and each gateway chat-completion is a child
span carrying `gen_ai.usage.input_tokens` / `output_tokens`, model, and provider.

Because the spans share a trace and link by `parent_span_id`, a request's **full**
token cost — summed across every completion in a multi-step tool/agent chain —
**rolls up the span tree**. That rollup is proven deterministically in
`tests/telemetry_token_spans.rs::multi_hop_agent_chain_rolls_up_token_usage_across_the_tree`
(request → agent.turn → tool → chat.completions, summed across depth). The same
totals show live in the `riz run --dev` **Tokens** panel (model · in→out) and
export via OTLP to Datadog / CloudWatch-X-Ray.

What riz ships today: request + chat-completion spans with token attrs, and the
tree-rollup mechanism. The intermediate `agent.turn` / `tool.*` spans in a fully
end-to-end agent trace are contributed by the agent/instrumentation layer (the
Agent SDK above drives the steps); riz makes every step's token cost attributable
to the originating request.
