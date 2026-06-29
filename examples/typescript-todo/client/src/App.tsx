import { useEffect, useMemo, useState } from "react";
import { api, type Todo } from "./api.ts";

type Filter = "all" | "active" | "completed";

export function App() {
  const [todos, setTodos] = useState<Todo[]>([]);
  const [filter, setFilter] = useState<Filter>("all");
  const [draft, setDraft] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // Initial load from the API (same origin, no CORS).
  useEffect(() => {
    api
      .list()
      .then(setTodos)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const remaining = todos.filter((t) => !t.completed).length;
  const completedCount = todos.length - remaining;

  const visible = useMemo(() => {
    if (filter === "active") return todos.filter((t) => !t.completed);
    if (filter === "completed") return todos.filter((t) => t.completed);
    return todos;
  }, [todos, filter]);

  // Run an API call, surface failures, and never leave a half-state.
  async function run<T>(p: Promise<T>, after: (r: T) => void) {
    try {
      after(await p);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  function add(e: React.FormEvent) {
    e.preventDefault();
    const title = draft.trim();
    if (!title) return;
    setDraft("");
    run(api.create(title), (todo) => setTodos((ts) => [...ts, todo]));
  }

  function toggle(todo: Todo) {
    run(api.update(todo.id, { completed: !todo.completed }), (updated) =>
      setTodos((ts) => ts.map((t) => (t.id === updated.id ? updated : t))),
    );
  }

  function remove(id: string) {
    run(api.remove(id), () => setTodos((ts) => ts.filter((t) => t.id !== id)));
  }

  function commitEdit(todo: Todo) {
    const title = editText.trim();
    setEditingId(null);
    if (!title) return remove(todo.id);
    if (title === todo.title) return;
    run(api.update(todo.id, { title }), (updated) =>
      setTodos((ts) => ts.map((t) => (t.id === updated.id ? updated : t))),
    );
  }

  function toggleAll() {
    const target = remaining > 0; // if any active remain, complete all; else reactivate all
    todos
      .filter((t) => t.completed !== target)
      .forEach((t) =>
        run(api.update(t.id, { completed: target }), (updated) =>
          setTodos((ts) => ts.map((x) => (x.id === updated.id ? updated : x))),
        ),
      );
  }

  function clearCompleted() {
    todos
      .filter((t) => t.completed)
      .forEach((t) =>
        run(api.remove(t.id), () =>
          setTodos((ts) => ts.filter((x) => x.id !== t.id)),
        ),
      );
  }

  return (
    <main className="app">
      <header className="masthead">
        <h1>todos</h1>
        <p className="sub">
          API + client on one <code>riz</code> binary, one origin — no CORS.
        </p>
      </header>

      <section className="card">
        <form className="new" onSubmit={add}>
          {todos.length > 0 && (
            <button
              type="button"
              className={`toggle-all ${remaining === 0 ? "on" : ""}`}
              onClick={toggleAll}
              aria-label="Toggle all"
              title="Toggle all"
            >
              ❯
            </button>
          )}
          <input
            className="new-input"
            placeholder="What needs to be done?"
            value={draft}
            autoFocus
            onChange={(e) => setDraft(e.target.value)}
          />
        </form>

        {error && <div className="error">{error}</div>}
        {loading ? (
          <div className="empty">Loading…</div>
        ) : todos.length === 0 ? (
          <div className="empty">Nothing yet. Add your first todo above.</div>
        ) : (
          <ul className="list">
            {visible.map((todo) => (
              <li key={todo.id} className={todo.completed ? "done" : ""}>
                <input
                  type="checkbox"
                  className="check"
                  checked={todo.completed}
                  onChange={() => toggle(todo)}
                />
                {editingId === todo.id ? (
                  <input
                    className="edit"
                    autoFocus
                    value={editText}
                    onChange={(e) => setEditText(e.target.value)}
                    onBlur={() => commitEdit(todo)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitEdit(todo);
                      if (e.key === "Escape") setEditingId(null);
                    }}
                  />
                ) : (
                  <span
                    className="title"
                    onDoubleClick={() => {
                      setEditingId(todo.id);
                      setEditText(todo.title);
                    }}
                  >
                    {todo.title}
                  </span>
                )}
                <button
                  className="del"
                  onClick={() => remove(todo.id)}
                  aria-label="Delete"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}

        {todos.length > 0 && (
          <footer className="foot">
            <span className="count">
              <strong>{remaining}</strong> {remaining === 1 ? "item" : "items"} left
            </span>
            <span className="filters">
              {(["all", "active", "completed"] as Filter[]).map((f) => (
                <button
                  key={f}
                  className={filter === f ? "on" : ""}
                  onClick={() => setFilter(f)}
                >
                  {f}
                </button>
              ))}
            </span>
            <button
              className="clear"
              onClick={clearCompleted}
              disabled={completedCount === 0}
            >
              Clear completed
            </button>
          </footer>
        )}
      </section>

      <footer className="colophon">
        Served by riz · <a href="/llms.txt">/llms.txt</a> ·{" "}
        <a href="/_riz/health">/_riz/health</a> · every function is also an MCP
        tool at <code>/_riz/mcp</code>
      </footer>
    </main>
  );
}
