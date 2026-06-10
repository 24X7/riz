// agent-tools — a small set of side-effect-light "tools" an LLM agent calls
// over riz's MCP endpoint. Each route below becomes ONE MCP tool the moment
// `riz run` boots (tool name == riz.toml function name).
//
// Data is in-memory and deterministic so the substrate is provable in CI
// without any model or network. The Claude Agent SDK demo in
// examples/agent-sdk/ drives these exact functions as tools.
//
// Routes (configured in examples/riz.agent.toml):
//   GET  /orders/{id}   → lookup_order   — order status + delay flag
//   GET  /inventory     → list_inventory — current stock levels
//   POST /tickets       → create_ticket  — open a support ticket (in-memory)

interface Order {
  id: string;
  item: string;
  status: "processing" | "shipped" | "delivered" | "delayed";
  delayed: boolean;
  etaDays: number;
}

// Deterministic seed data. Order 1042 is delayed on purpose — it's the
// canonical "look up 1042, open a ticket if it's delayed" demo path.
const ORDERS: Record<string, Order> = {
  "1041": { id: "1041", item: "Mechanical keyboard", status: "delivered", delayed: false, etaDays: 0 },
  "1042": { id: "1042", item: "27in monitor", status: "delayed", delayed: true, etaDays: 9 },
  "1043": { id: "1043", item: "USB-C hub", status: "shipped", delayed: false, etaDays: 2 },
};

const INVENTORY = [
  { sku: "KB-MECH-87", name: "Mechanical keyboard", inStock: 42 },
  { sku: "MON-27-4K", name: "27in monitor", inStock: 0 },
  { sku: "HUB-USBC-7", name: "USB-C hub", inStock: 130 },
];

// Support tickets opened this process lifetime. Resets on restart — local-dev.
interface Ticket {
  id: string;
  orderId: string;
  reason: string;
  createdAt: number;
}
const TICKETS = new Map<string, Ticket>();
let nextTicket = 5000;

function json(statusCode: number, payload: unknown) {
  return {
    statusCode,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  };
}

export const lookupOrder = async (event: any) => {
  const id: string | undefined = event.pathParameters?.id;
  if (!id) return json(400, { error: "id path parameter required" });
  const order = ORDERS[id];
  if (!order) return json(404, { error: `order ${id} not found` });
  return json(200, order);
};

export const listInventory = async (_event: any) => {
  return json(200, { items: INVENTORY, count: INVENTORY.length });
};

export const createTicket = async (event: any) => {
  let body: Record<string, unknown> = {};
  if (event.body) {
    try {
      body = JSON.parse(event.body);
    } catch {
      return json(400, { error: "body must be valid JSON" });
    }
  }
  const orderId = typeof body.orderId === "string" ? body.orderId : "";
  const reason = typeof body.reason === "string" ? body.reason : "unspecified";
  if (!orderId) return json(400, { error: "orderId is required" });

  const id = `T-${nextTicket++}`;
  const ticket: Ticket = { id, orderId, reason, createdAt: Date.now() };
  TICKETS.set(id, ticket);
  return json(201, { ticket, status: "open" });
};
