// The server-side agent loop: model → tool_calls → execute → tool results →
// final answer. Everything below runs through riz's OpenAI-compatible gateway
// (/_riz/v1), so switching mock → Anthropic/OpenAI/Ollama is a riz.toml edit —
// this file never changes.
//
// AWS API Gateway v2 HTTP Lambda handler; runs on riz via the embedded Bun
// runtime, and (like every riz function) is itself a typed MCP tool at
// /_riz/mcp that outside agents can call.

const GATEWAY_URL = process.env.GATEWAY_URL ?? "http://127.0.0.1:3000/_riz/v1";
const CHAT_MODEL = process.env.CHAT_MODEL ?? "mock";
const MAX_HOPS = 5;

// ── The tools this API hands the model ───────────────────────────────────────
// Plain local functions + their OpenAI tool schemas. Add one entry to give the
// model a new capability.

const ORDERS: Record<string, { status: string; eta: string }> = {
  "42": { status: "shipped", eta: "2 days" },
  "1337": { status: "packing", eta: "5 days" },
};

const TOOL_IMPLS: Record<string, (args: any) => string> = {
  get_time: () => new Date().toISOString(),
  lookup_order: (args) => {
    const order = ORDERS[String(args?.order_id ?? "")];
    return order
      ? JSON.stringify({ order_id: args.order_id, ...order })
      : JSON.stringify({ error: `no order ${args?.order_id}` });
  },
};

const TOOLS = [
  {
    type: "function",
    function: {
      name: "lookup_order",
      description: "Look up an order's status and ETA by id",
      parameters: {
        type: "object",
        properties: { order_id: { type: "string", description: "the order id" } },
        required: ["order_id"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "get_time",
      description: "Current server time (ISO 8601)",
      parameters: { type: "object", properties: {} },
    },
  },
];

// ── The loop ─────────────────────────────────────────────────────────────────

async function chatOnce(messages: any[]) {
  const resp = await fetch(`${GATEWAY_URL}/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ model: CHAT_MODEL, messages, tools: TOOLS }),
  });
  if (!resp.ok) {
    throw new Error(`gateway ${resp.status}: ${await resp.text()}`);
  }
  return resp.json();
}

async function runAgentLoop(messages: any[]) {
  const toolTrace: { name: string; arguments: string; result: string }[] = [];
  let usage = { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 };

  for (let hop = 0; hop < MAX_HOPS; hop++) {
    const data = await chatOnce(messages);
    const choice = data.choices[0];
    usage = {
      prompt_tokens: usage.prompt_tokens + (data.usage?.prompt_tokens ?? 0),
      completion_tokens: usage.completion_tokens + (data.usage?.completion_tokens ?? 0),
      total_tokens: usage.total_tokens + (data.usage?.total_tokens ?? 0),
    };
    messages.push(choice.message);

    const calls = choice.message.tool_calls ?? [];
    if (choice.finish_reason !== "tool_calls" || calls.length === 0) {
      return { reply: choice.message.content ?? "", toolTrace, usage };
    }
    for (const call of calls) {
      const impl = TOOL_IMPLS[call.function?.name];
      let result: string;
      try {
        const args = call.function?.arguments ? JSON.parse(call.function.arguments) : {};
        result = impl ? impl(args) : `unknown tool: ${call.function?.name}`;
      } catch (e) {
        result = `tool error: ${e}`;
      }
      toolTrace.push({
        name: call.function?.name ?? "?",
        arguments: call.function?.arguments ?? "{}",
        result,
      });
      messages.push({ role: "tool", tool_call_id: call.id, content: result });
    }
  }
  return { reply: "(agent loop hit the hop limit)", toolTrace, usage };
}

// ── The handler ──────────────────────────────────────────────────────────────

export const handler = async (event: any) => {
  let body: any;
  try {
    body = JSON.parse(event.body ?? "{}");
  } catch {
    return { statusCode: 400, body: JSON.stringify({ error: "invalid JSON body" }) };
  }
  const incoming = Array.isArray(body.messages) ? body.messages : [];
  if (incoming.length === 0) {
    return { statusCode: 400, body: JSON.stringify({ error: "messages[] required" }) };
  }

  const messages = [
    {
      role: "system",
      content:
        "You are a concise support assistant for a small shop. Use the tools to answer order questions.",
    },
    ...incoming,
  ];

  try {
    const { reply, toolTrace, usage } = await runAgentLoop(messages);
    return {
      statusCode: 200,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ reply, tool_trace: toolTrace, usage, model: CHAT_MODEL }),
    };
  } catch (e) {
    return { statusCode: 502, body: JSON.stringify({ error: String(e) }) };
  }
};
