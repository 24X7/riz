// Tiny typed client for the riz todo function. Every call hits the SAME origin
// the app is served from (`/api/...`), so there is no CORS and no base URL to
// configure — riz serves this bundle and answers these routes on one binary.

export interface Todo {
  id: string;
  title: string;
  completed: boolean;
  createdAt: number;
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    throw new Error(`${init?.method ?? "GET"} ${path} → ${res.status}`);
  }
  // 204 No Content (delete) has no body.
  return res.status === 204 ? (undefined as T) : ((await res.json()) as T);
}

export const api = {
  list: () => req<Todo[]>("/api/todos"),
  create: (title: string) =>
    req<Todo>("/api/todos", {
      method: "POST",
      body: JSON.stringify({ title }),
    }),
  update: (id: string, patch: Partial<Pick<Todo, "title" | "completed">>) =>
    req<Todo>(`/api/todos/${id}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),
  remove: (id: string) =>
    req<void>(`/api/todos/${id}`, { method: "DELETE" }),
};
