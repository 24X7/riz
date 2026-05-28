// CRUD example — demonstrates all five HTTP verbs on a /accounts resource.
// In-memory storage resets on process restart; this is a local-dev example.
//
// Routes (configured in examples/riz.dev.toml):
//   GET    /accounts/{id}   → 200 with stored account or 404
//   POST   /accounts        → 201 with new account id
//   PUT    /accounts/{id}   → 200 replacing the account
//   PATCH  /accounts/{id}   → 200 with partial update applied
//   DELETE /accounts/{id}   → 204 no content

interface Account {
  id: string;
  name: string;
  plan: string;
  email?: string;
  createdAt: number;
  updatedAt: number;
}

// Shared in-memory store. Persists for the lifetime of the process pool.
const store = new Map<string, Account>();

let nextId = 1;

function generateId(): string {
  return String(nextId++);
}

function notFound(id: string) {
  return {
    statusCode: 404,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ error: `account ${id} not found` }),
  };
}

function badRequest(msg: string) {
  return {
    statusCode: 400,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ error: msg }),
  };
}

export const handler = async (event: any) => {
  const method: string = event.requestContext?.http?.method ?? "GET";
  const id: string | undefined = event.pathParameters?.id;

  // Parse body once — needed for POST / PUT / PATCH.
  let body: Record<string, unknown> = {};
  if (event.body) {
    try {
      body = JSON.parse(event.body);
    } catch {
      return badRequest("body must be valid JSON");
    }
  }

  switch (method) {
    case "GET": {
      if (!id) return badRequest("id path parameter required");
      const account = store.get(id);
      if (!account) return notFound(id);
      return {
        statusCode: 200,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(account),
      };
    }

    case "POST": {
      const newId = generateId();
      const now = Date.now();
      const account: Account = {
        id: newId,
        name: typeof body.name === "string" ? body.name : `Account ${newId}`,
        plan: typeof body.plan === "string" ? body.plan : "free",
        email: typeof body.email === "string" ? body.email : undefined,
        createdAt: now,
        updatedAt: now,
      };
      store.set(newId, account);
      return {
        statusCode: 201,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(account),
      };
    }

    case "PUT": {
      if (!id) return badRequest("id path parameter required");
      if (!store.has(id)) return notFound(id);
      const now = Date.now();
      const account: Account = {
        id,
        name: typeof body.name === "string" ? body.name : `Account ${id}`,
        plan: typeof body.plan === "string" ? body.plan : "free",
        email: typeof body.email === "string" ? body.email : undefined,
        createdAt: store.get(id)!.createdAt,
        updatedAt: now,
      };
      store.set(id, account);
      return {
        statusCode: 200,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(account),
      };
    }

    case "PATCH": {
      if (!id) return badRequest("id path parameter required");
      const existing = store.get(id);
      if (!existing) return notFound(id);
      const updated: Account = {
        ...existing,
        ...(typeof body.name === "string" ? { name: body.name } : {}),
        ...(typeof body.plan === "string" ? { plan: body.plan } : {}),
        ...(typeof body.email === "string" ? { email: body.email } : {}),
        updatedAt: Date.now(),
      };
      store.set(id, updated);
      return {
        statusCode: 200,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(updated),
      };
    }

    case "DELETE": {
      if (!id) return badRequest("id path parameter required");
      if (!store.has(id)) return notFound(id);
      store.delete(id);
      return { statusCode: 204, body: "" };
    }

    default:
      return {
        statusCode: 405,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ error: `method ${method} not allowed` }),
      };
  }
};
