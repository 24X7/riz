// Todo API — the backend for the colocated TodoMVC client (see ../README.md).
//
// One riz function, four routes (the AWS "one pool, many routes" shape). The
// React client in ../client calls these on the SAME origin riz serves it from,
// so there is no CORS and no second host — that is the whole point of the
// [static] block in ../riz.toml.
//
// Routes (configured in ../riz.toml):
//   GET    /api/todos        → 200 list all todos
//   POST   /api/todos        → 201 create { title }            (+ optional completed)
//   PATCH  /api/todos/{id}   → 200 partial update { title?, completed? }
//   DELETE /api/todos/{id}   → 204 delete one
//
// Storage is in-memory and resets when the process pool restarts — this is a
// local-dev demo, not a database example.

interface Todo {
  id: string;
  title: string;
  completed: boolean;
  createdAt: number;
}

// Shared in-memory store, ordered by insertion (Map preserves it).
const store = new Map<string, Todo>();
let nextId = 1;

const JSON_HEADERS = { "content-type": "application/json" };

function json(statusCode: number, value: unknown) {
  return { statusCode, headers: JSON_HEADERS, body: JSON.stringify(value) };
}

function badRequest(msg: string) {
  return json(400, { error: msg });
}

function notFound(id: string) {
  return json(404, { error: `todo ${id} not found` });
}

export const handler = async (event: any) => {
  const method: string = event.requestContext?.http?.method ?? "GET";
  const id: string | undefined = event.pathParameters?.id;

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
      // Whole collection, in creation order.
      return json(200, [...store.values()]);
    }

    case "POST": {
      const title = typeof body.title === "string" ? body.title.trim() : "";
      if (!title) return badRequest("title is required");
      const newId = String(nextId++);
      const todo: Todo = {
        id: newId,
        title,
        completed: typeof body.completed === "boolean" ? body.completed : false,
        createdAt: Date.now(),
      };
      store.set(newId, todo);
      return json(201, todo);
    }

    case "PATCH": {
      if (!id) return badRequest("id path parameter required");
      const existing = store.get(id);
      if (!existing) return notFound(id);
      const updated: Todo = {
        ...existing,
        ...(typeof body.title === "string" && body.title.trim()
          ? { title: body.title.trim() }
          : {}),
        ...(typeof body.completed === "boolean"
          ? { completed: body.completed }
          : {}),
      };
      store.set(id, updated);
      return json(200, updated);
    }

    case "DELETE": {
      if (!id) return badRequest("id path parameter required");
      if (!store.delete(id)) return notFound(id);
      return { statusCode: 204, body: "" };
    }

    default:
      return json(405, { error: `method ${method} not allowed` });
  }
};
