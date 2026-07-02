import { useEffect, useRef, useState } from "react";

type ToolStep = { name: string; arguments: string; result: string };
type Turn = {
  role: "user" | "assistant";
  content: string;
  toolTrace?: ToolStep[];
};

async function sendChat(history: { role: string; content: string }[]) {
  const r = await fetch("/api/chat", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ messages: history }),
  });
  if (!r.ok) throw new Error(`API ${r.status}: ${await r.text()}`);
  return r.json() as Promise<{
    reply: string;
    tool_trace: ToolStep[];
    usage: { total_tokens: number };
    model: string;
  }>;
}

export function App() {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [model, setModel] = useState<string | null>(null);
  const [tokens, setTokens] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [turns, busy]);

  async function submit() {
    const text = draft.trim();
    if (!text || busy) return;
    setError(null);
    setDraft("");
    const nextTurns: Turn[] = [...turns, { role: "user", content: text }];
    setTurns(nextTurns);
    setBusy(true);
    try {
      const history = nextTurns.map(({ role, content }) => ({ role, content }));
      const res = await sendChat(history);
      setModel(res.model);
      setTokens((t) => t + (res.usage?.total_tokens ?? 0));
      setTurns([
        ...nextTurns,
        { role: "assistant", content: res.reply, toolTrace: res.tool_trace },
      ]);
    } catch (e) {
      setError(String(e));
      setTurns(nextTurns);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app">
      <header>
        <h1>
          riz<span className="cursor">▮</span> <span className="dim">· ai chat</span>
        </h1>
        <div className="badges">
          {model && <span className="badge">model: {model}</span>}
          {tokens > 0 && <span className="badge">{tokens} tokens</span>}
        </div>
      </header>

      <main>
        {turns.length === 0 && (
          <div className="hint">
            <p>
              One binary is serving this page, the <code>/api/chat</code> function behind
              it, the LLM gateway it calls, and an MCP endpoint for outside agents.
            </p>
            <p>
              Try: <button className="chip" onClick={() => setDraft("where is order 42?")}>
                where is order 42?
              </button>{" "}
              <button className="chip" onClick={() => setDraft("what time is it on the server?")}>
                what time is it on the server?
              </button>
            </p>
          </div>
        )}

        {turns.map((t, i) => (
          <div key={i} className={`turn ${t.role}`}>
            {t.toolTrace && t.toolTrace.length > 0 && (
              <div className="tools">
                {t.toolTrace.map((s, j) => (
                  <details key={j} className="tool">
                    <summary>
                      ⚙ <code>{s.name}({s.arguments})</code>
                    </summary>
                    <pre>{s.result}</pre>
                  </details>
                ))}
              </div>
            )}
            <div className="bubble">{t.content}</div>
          </div>
        ))}

        {busy && <div className="turn assistant"><div className="bubble dim">…</div></div>}
        {error && <div className="error">{error}</div>}
        <div ref={endRef} />
      </main>

      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="Ask about an order…"
          disabled={busy}
          autoFocus
        />
        <button type="submit" disabled={busy || !draft.trim()}>
          send
        </button>
      </form>

      <footer>
        served by one riz binary — <code>/api/chat</code> is also an MCP tool:{" "}
        <a href="/_riz/mcp">/_riz/mcp</a> · <a href="/_riz/health">/_riz/health</a> ·{" "}
        <a href="https://riz.dev" target="_blank" rel="noreferrer">riz.dev</a>
      </footer>
    </div>
  );
}
