#!/usr/bin/env python3
"""riz — complete narrated walkthrough of EVERY capability (Python orchestrator).

Boots ONE riz instance from examples/riz.all.toml and demonstrates, live:

  • System surface          /ready, /_riz/registry, /_riz/health, /_riz/metrics
  • MCP server (2025-11-25)  raw JSON-RPC wire test + built-in inspector
  • LLM gateway (live)       OpenAI-compatible /_riz/v1 → a REAL local model via
                             Ollama (llama3.2:1b), plus the mock provider
  • All FIVE runtimes        Bun, Node.js, Python, Rust, WASM — one envelope
  • Capability-sandboxed WASM  a wasm32-wasip1 handler run under wasmtime
  • HTTP shapes              path params, query string, JSON body, every verb (CRUD)
  • Response caching         a cache HIT replays the response; invalidate evicts
  • CORS                     OPTIONS preflight + Access-Control-Allow-Origin
  • Stage variables          per-function config surfaced on the event
  • On-box safety            per-function rlimit/Landlock caps + always-on profile
  • Lambda authorizers       REQUEST allow → 200, deny → 401
  • WebSocket                $connect/$default/$disconnect across Bun/Python/Rust
  • Handler hot-reload        edit a handler, next request runs new code (no restart)
  • Scaffolding & doctor      riz init --list, riz doctor

Everything is REAL: every line below makes an actual HTTP/JSON-RPC/WebSocket call
to the running server. No mocked output, no canned strings. This is the SHOWCASE
script; examples/smoke-all.sh is the terse assertion-style CI companion.

Tables render with `rich` (pip install rich) — width-aware, no scattered columns;
the demo degrades to plain text if rich is absent. Everything else is stdlib
(urllib, subprocess, socket). The WebSocket round-trip is implemented natively
here, so websocat is no longer required.

Prereqs:
  - cargo build --release   (produces target/release/{riz,echo-rust,chat-rust})
  - bun, node, python3 on PATH                (TS / JS / Python handlers)
  - ollama                                    (live local-model gateway demo;
                                               soft-skipped if missing —
                                               install: brew install --cask ollama-app)
  - rustup target add wasm32-wasip1           (the WASM handler; soft-skipped)

Knobs:
  PAUSE=1 python3 examples/demo.py     # pause for ENTER between sections
  NO_COLOR=1 python3 examples/demo.py  # disable color

Usage:  python3 examples/demo.py   (or ./examples/demo.py)
"""

from __future__ import annotations

import base64
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

try:
    import termios  # unix only — used to restore the TTY if a child mangles it
except ImportError:  # pragma: no cover — non-unix
    termios = None

# Subprocesses must never touch our controlling terminal. `ollama serve` (and
# friends) inherit stdin and flip the TTY to raw mode (ONLCR off → every print
# stair-steps to the right and never returns to column 0). Detach stdin so no
# child sees a TTY; DEFAULT keyword threaded into every spawn below.
DEVNULL = subprocess.DEVNULL

_TTY_FD = None
_TTY_SAVED = None


def save_tty() -> None:
    global _TTY_FD, _TTY_SAVED
    if termios is None or not sys.stdout.isatty():
        return
    try:
        _TTY_FD = sys.stdin.fileno()
        _TTY_SAVED = termios.tcgetattr(_TTY_FD)
    except Exception:  # noqa: BLE001
        _TTY_SAVED = None


def restore_tty() -> None:
    if termios is not None and _TTY_SAVED is not None:
        try:
            termios.tcsetattr(_TTY_FD, termios.TCSANOW, _TTY_SAVED)
        except Exception:  # noqa: BLE001
            pass

# ─────────────────────────── Paths & constants ───────────────────────────
ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "riz"
CFG = ROOT / "examples" / "riz.all.toml"
HOST, PORT = "127.0.0.1", 3000
BASE = f"http://{HOST}:{PORT}"
LOG = Path("/tmp/riz-demo.log")
OLLAMA_MODEL = "llama3.2:1b"
PAUSE = os.environ.get("PAUSE") == "1"

# ──────────────────────────── Pretty output ──────────────────────────────
_color = sys.stdout.isatty() and not os.environ.get("NO_COLOR")

# Wrap everything to the ACTUAL terminal width so nothing scatters across a
# wide screen or wraps mid-row on a narrow/large-font one. Content wraps a few
# columns short of the edge to leave room for the 6-space indent.
TERM_W = shutil.get_terminal_size((80, 24)).columns
WRAP = max(44, min(TERM_W - 4, 76))


def _c(code: str) -> str:
    return f"\033[{code}m" if _color else ""


BOLD, DIM = _c("1"), _c("2")
CYAN, AMBER, GREEN, RED = _c("36"), _c("33"), _c("32"), _c("31")
RESET = _c("0")

# Tabular sections render with `rich` (pip install rich) — width-aware, wraps
# cells instead of scattering columns. Degrades to a plain aligned table if
# rich isn't installed, so the demo still runs anywhere.
try:
    from rich import box
    from rich.console import Console
    from rich.table import Table

    _console: "Console | None" = Console()
except ImportError:  # pragma: no cover
    _console = None


def render_table(columns: list[str], rows: list[list[str]], indent_n: int = 2) -> None:
    """Print a tidy table. Uses rich when available; falls back to aligned text."""
    if _console is not None:
        t = Table(box=box.SIMPLE_HEAD, show_edge=False, pad_edge=False,
                  padding=(0, 2, 0, 0), header_style="bold cyan")
        for col in columns:
            t.add_column(col, overflow="fold", no_wrap=False)
        for r in rows:
            t.add_row(*r)
        with _console.capture() as cap:
            _console.print(t)
        pad = " " * indent_n
        print("\n".join(pad + ln for ln in cap.get().rstrip("\n").splitlines()))
        return
    # Fallback: aligned plain-text table.
    head = [columns] + rows
    widths = [max(len(r[i]) for r in head) for i in range(len(columns))]
    pad = " " * indent_n
    for i, r in enumerate(head):
        line = "  ".join(c.ljust(widths[j]) for j, c in enumerate(r)).rstrip()
        print(pad + line)
        if i == 0:
            print(pad + "  ".join("─" * w for w in widths))


def banner(title: str) -> None:
    print(f"\n{BOLD}{AMBER}━━━ {title} ━━━{RESET}")


def sub(text: str) -> None:  # narration
    print(f"{DIM}{text}{RESET}")


def req(text: str) -> None:  # the request line
    print(f"  {CYAN}{text}{RESET}")


def out(text: str) -> None:  # its result, indented
    print(f"      {text}")


def kv(label: str, value: str, width: int = 22) -> None:  # aligned label → value
    print(f"  {CYAN}{label:<{width}}{RESET} {value}")


def ok(text: str) -> None:
    print(f"  {GREEN}✓{RESET} {text}")


def warn(text: str) -> None:
    print(f"  {AMBER}!{RESET} {text}")


def indent(block: str, n: int = 6) -> str:
    pad = " " * n
    return "\n".join(pad + line for line in block.splitlines())


def fold(text: str, width: int | None = None) -> list[str]:
    """Word-wrap a string to <=width-char lines (for model prose)."""
    width = width or WRAP
    words, lines, cur = text.split(), [], ""
    for w in words:
        if cur and len(cur) + 1 + len(w) > width:
            lines.append(cur)
            cur = w
        else:
            cur = f"{cur} {w}" if cur else w
    if cur:
        lines.append(cur)
    return lines or [""]


def wrapped(items: list[str], indent_n: int = 6, sep: str = "  ") -> None:
    """Print a list of short tokens wrapped to the terminal width, indented."""
    pad = " " * indent_n
    cur = ""
    for it in items:
        cand = it if not cur else cur + sep + it
        if cur and indent_n + len(cand) > WRAP:
            print(pad + cur)
            cur = it
        else:
            cur = cand
    if cur:
        print(pad + cur)


def pause() -> None:
    if PAUSE:
        try:
            input(f"{DIM}[press ENTER to continue]{RESET} ")
        except EOFError:
            pass


def die(msg: str) -> "None":
    print(f"{RED}error:{RESET} {msg}", file=sys.stderr)
    sys.exit(1)


# ──────────────────────────── HTTP helpers ───────────────────────────────
def http(method: str, path: str, data=None, headers=None, base: str = BASE, timeout: float = 20.0):
    """Returns (status:int|None, body:str, headers:dict)."""
    body = data.encode() if isinstance(data, str) else data
    request = urllib.request.Request(base + path, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace"), {k.lower(): v for k, v in r.headers.items()}
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), {k.lower(): v for k, v in e.headers.items()}
    except Exception as e:  # noqa: BLE001 — soft for a live demo
        return None, str(e), {}


def get(path: str):
    return http("GET", path)


def getj(path: str):
    _, body, _ = get(path)
    return json.loads(body)


def post_json(path: str, obj, base: str = BASE):
    return http("POST", path, json.dumps(obj), {"content-type": "application/json"}, base=base)


def post_jsonj(path: str, obj):
    _, body, _ = post_json(path, obj)
    return json.loads(body)


def reachable(url: str) -> bool:
    try:
        urllib.request.urlopen(url, timeout=2).read()
        return True
    except Exception:  # noqa: BLE001
        return False


# ───────────────────── Native WebSocket round-trip ───────────────────────
def ws_roundtrip(path: str, message: str, timeout: float = 4.0):
    """Open a WS connection (fires $connect), send one text frame ($default),
    read one reply frame, close ($disconnect). Pure stdlib. Returns str|None."""
    try:
        s = socket.create_connection((HOST, PORT), timeout=timeout)
        s.settimeout(timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        handshake = (
            f"GET {path} HTTP/1.1\r\nHost: {HOST}:{PORT}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        s.sendall(handshake.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = s.recv(4096)
            if not chunk:
                s.close()
                return None
            buf += chunk
        if b" 101 " not in buf.split(b"\r\n", 1)[0] + b" ":
            s.close()
            return None
        leftover = buf.split(b"\r\n\r\n", 1)[1]

        # Client→server frame: FIN + text, masked (required for clients).
        payload = message.encode()
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        n = len(payload)
        header = b"\x81"
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", n)
        s.sendall(header + mask + masked)

        def recvn(want: int, pre: bytes) -> bytes:
            data = pre
            while len(data) < want:
                chunk = s.recv(want - len(data))
                if not chunk:
                    break
                data += chunk
            return data

        # Server→client frame is unmasked.
        frame = recvn(2, leftover)
        if len(frame) < 2:
            s.close()
            return None
        ln, rest = frame[1] & 0x7F, frame[2:]
        if ln == 126:
            ext = recvn(2, rest)
            ln, rest = struct.unpack(">H", ext[:2])[0], ext[2:]
        elif ln == 127:
            ext = recvn(8, rest)
            ln, rest = struct.unpack(">Q", ext[:8])[0], ext[8:]
        data = recvn(ln, rest)[:ln]
        s.close()
        return data.decode("utf-8", "replace")
    except Exception:  # noqa: BLE001
        return None


# ──────────────────────────── Process state ──────────────────────────────
class State:
    riz: subprocess.Popen | None = None
    ollama: subprocess.Popen | None = None  # only if WE started it
    ollama_bin: str | None = None
    ollama_ready = False
    has_wasm = False
    cfg_run = CFG
    ping_src = ROOT / "examples" / "lambdas" / "ping" / "index.ts"
    ping_backup: bytes | None = None


ST = State()


def cleanup() -> None:
    if ST.riz and ST.riz.poll() is None:
        ST.riz.terminate()
        try:
            ST.riz.wait(timeout=5)
        except subprocess.TimeoutExpired:
            ST.riz.kill()
    if ST.ollama and ST.ollama.poll() is None:
        ST.ollama.terminate()
    if ST.ping_backup is not None:
        try:
            ST.ping_src.write_bytes(ST.ping_backup)
        except OSError:
            pass
    if ST.cfg_run != CFG:
        try:
            Path(ST.cfg_run).unlink()
        except OSError:
            pass
    restore_tty()  # safety net in case a child still mangled the terminal


# ════════════════════════════ Demo sections ═════════════════════════════
def build_wasm() -> None:
    banner("Building the WASM handler (wasm32-wasip1)")
    wasm_dir = ROOT / "examples" / "lambdas" / "echo-wasm"
    wasm_out = wasm_dir / "target" / "wasm32-wasip1" / "release" / "echo-wasm.wasm"
    if wasm_out.exists():
        ST.has_wasm = True
        ok(f"already built: {wasm_out.relative_to(ROOT)}")
    elif _wasm_target_installed():
        sub("cargo build --release --target wasm32-wasip1  (in examples/lambdas/echo-wasm)")
        r = subprocess.run(
            ["cargo", "build", "--release", "--target", "wasm32-wasip1"],
            cwd=wasm_dir, capture_output=True, text=True, stdin=DEVNULL,
        )
        if r.returncode == 0 and wasm_out.exists():
            ST.has_wasm = True
            ok(f"built {wasm_out.relative_to(ROOT)}")
        else:
            Path("/tmp/riz-wasm-build.log").write_text(r.stdout + r.stderr)
            warn("wasm build failed (see /tmp/riz-wasm-build.log) — /echo-wasm will be skipped")
    else:
        warn("wasm32-wasip1 target not installed — run: rustup target add wasm32-wasip1")
        warn("the /echo-wasm runtime will be skipped (every other runtime still runs)")

    # If the module is missing, boot a temp config with echo-wasm stripped so
    # riz doctor/boot stays clean. riz reads any path as TOML; ext is irrelevant.
    if not ST.has_wasm:
        lines, skip = [], False
        for line in CFG.read_text().splitlines():
            if line.startswith("[function.echo-wasm]"):
                skip = True
            elif line.startswith("[function.auth-allow]"):
                skip = False
            if not skip:
                lines.append(line)
        tmp = Path("/tmp/riz-demo-nowasm.toml")
        tmp.write_text("\n".join(lines) + "\n")
        ST.cfg_run = tmp


def _wasm_target_installed() -> bool:
    try:
        r = subprocess.run(["rustup", "target", "list", "--installed"], capture_output=True, text=True, stdin=DEVNULL)
        return "wasm32-wasip1" in r.stdout
    except FileNotFoundError:
        return False


def boot() -> None:
    banner("Booting riz")
    count = 16 if ST.has_wasm else 15
    runtimes = "Bun · Node.js · Python · Rust · WASM" if ST.has_wasm else "Bun · Node.js · Python · Rust"
    sub(f"config: examples/riz.all.toml  ({count} functions across {runtimes})")

    # Free port 3000 if a stale instance is lingering.
    subprocess.run(
        "lsof -ti :3000 2>/dev/null | xargs -r kill -TERM 2>/dev/null || true",
        shell=True, stdin=DEVNULL,
    )
    time.sleep(1)

    logf = LOG.open("wb")
    ST.riz = subprocess.Popen(
        [str(BIN), "--log-level", "warn", "--config", str(ST.cfg_run), "run"],
        stdout=logf, stderr=subprocess.STDOUT, stdin=DEVNULL,
    )
    sys.stdout.write(f"{DIM}waiting for /ready {RESET}")
    sys.stdout.flush()
    for _ in range(30):
        if reachable(f"{BASE}/ready"):
            print(f"{GREEN}✓{RESET}")
            break
        sys.stdout.write(".")
        sys.stdout.flush()
        time.sleep(1)
    else:
        die("riz did not become ready — see " + str(LOG))
    time.sleep(2)  # let bun/node/python/rust/wasm workers finish spawning
    sub(f"logs streaming to {LOG}  (pid {ST.riz.pid})")
    pause()


def warm_ollama() -> None:
    # Resolve a WORKING ollama binary. The Homebrew *formula* has historically
    # shipped without its `llama-server` inference runner (so `ollama serve`
    # starts but every inference 500s). The official prebuilt app bundles the
    # runner, so prefer it; otherwise fall back to ollama on PATH.
    app = "/Applications/Ollama.app/Contents/Resources/ollama"
    if os.access(app, os.X_OK):
        ST.ollama_bin = app
    else:
        from shutil import which
        ST.ollama_bin = which("ollama")

    if not ST.ollama_bin:
        warn("ollama not found — live-model gateway demo falls back to mock")
        warn("(install: brew install --cask ollama-app — bundles the llama-server runner)")
        return

    banner(f"Warming up Ollama (live local model: {OLLAMA_MODEL})")
    sub(f"ollama binary: {ST.ollama_bin}")
    tags = "http://127.0.0.1:11434/api/tags"
    if not reachable(tags):
        sub("starting 'ollama serve' (background)…")
        ST.ollama = subprocess.Popen(
            [ST.ollama_bin, "serve"],
            stdout=open("/tmp/riz-ollama.log", "wb"), stderr=subprocess.STDOUT, stdin=DEVNULL,
        )
        for _ in range(30):
            if reachable(tags):
                break
            time.sleep(1)
    else:
        ok("ollama already serving on :11434")

    if not reachable(tags):
        warn("ollama did not come up — gateway demo will use the mock provider")
        pause()
        return

    have = subprocess.run([ST.ollama_bin, "list"], capture_output=True, text=True, stdin=DEVNULL).stdout
    if OLLAMA_MODEL in have:
        ok(f"model {OLLAMA_MODEL} present")
    else:
        sub(f"pulling {OLLAMA_MODEL} (first run only)…")
        r = subprocess.run([ST.ollama_bin, "pull", OLLAMA_MODEL], capture_output=True, text=True, stdin=DEVNULL)
        if r.returncode == 0:
            ok(f"pulled {OLLAMA_MODEL}")
        else:
            warn("pull failed")

    # Verify a REAL inference works (the formula-without-runner case 500s here).
    status, _, _ = post_json(
        "/v1/chat/completions",
        {"model": OLLAMA_MODEL, "messages": [{"role": "user", "content": "hi"}], "stream": False},
        base="http://127.0.0.1:11434",
    )
    if status == 200:
        ST.ollama_ready = True
        ok(f"live inference verified — gateway will route ollama/{OLLAMA_MODEL} to a real model")
    else:
        warn("ollama is up but inference failed (often a brew *formula* missing its llama-server")
        warn("runner — install the prebuilt app: brew install --cask ollama-app). Using mock.")
    pause()


def system_surface() -> None:
    banner("System surface — exposed by every riz instance, zero config")

    status, body, _ = get("/ready")
    req("GET /ready")
    out(f"HTTP {status}  {body.strip()}")

    reg = getj("/_riz/registry")
    sub(f"/_riz/registry — every function (system + user), {len(reg['functions'])} total:")
    rows = [[f.get("runtime") or "system", f["name"], ", ".join(f.get("routes", []))]
            for f in reg["functions"]]
    render_table(["Runtime", "Function", "Routes"], rows)

    h = getj("/_riz/health")
    healthy = sum(1 for f in h["functions"] if f["healthy"])
    req("GET /_riz/health")
    out(f"{healthy}/{len(h['functions'])} functions healthy · uptime {h['uptime_secs']}s · riz {h['version']}")

    _, metrics, _ = get("/_riz/metrics")
    counters = sum(1 for line in metrics.splitlines() if line.startswith("riz_invocations_total"))
    req("GET /_riz/metrics")
    out(f"Prometheus exposition · {counters} invocation counters + error & latency series")
    pause()


def mcp_section() -> None:
    banner("MCP server — /_riz/mcp speaks JSON-RPC 2.0 (spec 2025-11-25)")
    sub("Raw JSON-RPC — exactly what Claude Code / Cursor / the MCP Inspector send.")

    def mcp(payload):
        return post_jsonj("/_riz/mcp", payload)

    init = mcp({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "demo", "version": "0.1.0"}},
    })["result"]
    req("initialize")
    out(f"protocol {init['protocolVersion']} · server {init['serverInfo']['name']} {init['serverInfo']['version']}")

    tools = mcp({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})["result"]["tools"]
    req("tools/list")
    out(f"{len(tools)} tools, one per function (auto-exposed):")
    wrapped([t["name"] for t in tools])

    call = mcp({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "ping", "arguments": {}}})["result"]["structuredContent"]
    req("tools/call ping")
    out(f"statusCode {call['statusCode']} · body {call['body']}")

    sub("Built-in self-validating client (no external deps):")
    r = subprocess.run([str(BIN), "mcp", "inspect"], capture_output=True, text=True, stdin=DEVNULL)
    inspect_lines = (r.stdout + r.stderr).splitlines()[:4]
    print(indent("\n".join(inspect_lines)))
    pause()


def gateway_section() -> None:
    banner("LLM gateway — OpenAI-compatible API at /_riz/v1")
    sub("Point any OpenAI client here. Two providers wired: 'mock' (deterministic)")
    sub("and 'ollama' (a REAL local model, no key). Route by model prefix + fallback.")

    models = getj("/_riz/v1/models")
    req("GET /v1/models")
    out("providers: " + ", ".join(m["id"] for m in models["data"]))

    r = post_jsonj("/_riz/v1/chat/completions",
                   {"model": "mock", "messages": [{"role": "user", "content": "hello riz"}], "stream": False})
    req("POST /v1/chat/completions   (model = mock)")
    out(f"→ {r['choices'][0]['message']['content']}   [{r['usage']['total_tokens']} tok]")

    if ST.ollama_ready:
        r = post_jsonj("/_riz/v1/chat/completions", {
            "model": f"ollama/{OLLAMA_MODEL}",
            "messages": [{"role": "user", "content": "In one short sentence, what is AWS Lambda?"}],
            "stream": False,
        })
        content = r["choices"][0]["message"]["content"].strip()
        req(f"POST /v1/chat/completions   (model = ollama/{OLLAMA_MODEL} — REAL model)")
        prose = fold(content)
        out(f"→ {prose[0]}")
        for line in prose[1:]:
            out(f"  {line}")
        ok(f"live local inference · {r['usage']['prompt_tokens']}→{r['usage']['completion_tokens']} tokens attributed")
    else:
        warn("ollama not ready — gateway falls back to the mock provider")

    _, sse, _ = post_json("/_riz/v1/chat/completions",
                          {"model": "mock", "messages": [{"role": "user", "content": "stream me"}], "stream": True})
    events = sum(1 for line in sse.splitlines() if line.startswith("data:"))
    req("POST /v1/chat/completions   (stream = true)")
    out(f"{events} Server-Sent Events streamed (chat.completion.chunk … [DONE])")

    emb = post_jsonj("/_riz/v1/embeddings", {"model": "mock", "input": ["the quick brown fox", "lorem ipsum"]})
    req("POST /v1/embeddings")
    out(f"{len(emb['data'])} vectors × {len(emb['data'][0]['embedding'])} dims")

    usage = getj("/_riz/v1/usage")
    parts = [f"{k}: {v['requests']} req, ${v['cost_usd']}" for k, v in usage["providers"].items()]
    req("GET /v1/usage   (AI-FinOps)")
    out("   ·   ".join(parts))
    sub("Set [gateway] budget_usd to cap spend — over-budget requests return HTTP 412.")

    try:
        from openai import OpenAI
        client = OpenAI(base_url=f"{BASE}/_riz/v1", api_key="not-needed")
        resp = client.chat.completions.create(
            model="mock", messages=[{"role": "user", "content": "hi from the openai client"}], stream=False)
        req('OpenAI(base_url=".../_riz/v1").chat.completions.create(model="mock", …)')
        out(f"→ {resp.choices[0].message.content}")
    except ImportError:
        sub("(official openai python client speaks this exact wire; pip install openai to see it)")
    pause()


def runtimes_section() -> None:
    banner("Five runtimes — one Lambda envelope, identical responses")
    sub("The same GET hits five handlers in five languages; each returns the same shape.")

    def erow(name: str, runtime: str, path: str, extra=None):
        r = getj(f"{path}?name=alice")
        value = f"{runtime} · method {r['method']}"
        if extra:
            value += " · " + extra(r)
        kv(name, value, 12)

    erow("echo-bun", "Bun", "/echo-bun")
    erow("echo-node", "Node.js", "/echo-node")
    erow("echo-python", "Python", "/echo-python")
    erow("echo-rust", "Rust", "/echo-rust")
    if ST.has_wasm:
        erow("echo-wasm", "WASM", "/echo-wasm", lambda r: f"sandbox={r['stageVariables']['sandbox']}")
    else:
        warn("echo-wasm skipped (wasm module not built — see the build step above)")
    pause()


def http_shapes_section() -> None:
    banner("HTTP request shapes")
    req("GET /accounts/42?include=profile   (path param {id}=42 + query)")
    out(json.dumps(getj("/accounts/42?include=profile"), separators=(",", ":")))

    req("POST /events   (JSON body)")
    out(json.dumps(post_jsonj("/events", {"event": "login", "user": "alice"}), separators=(",", ":")))

    sub("crud-accounts — one handler, five verbs:")
    created = post_jsonj("/accounts", {"name": "alice", "plan": "pro"})
    out("POST   /accounts → " + json.dumps({k: created.get(k) for k in ("id", "name", "plan")}, separators=(",", ":")))
    out(f"GET    /crud/1   → HTTP {get('/crud/1')[0]}")
    out(f"DELETE /crud/1   → HTTP {http('DELETE', '/crud/1')[0]}")
    pause()


def caching_section() -> None:
    banner("Response caching — GET /accounts/{id} cached 30s, then invalidated")
    sub("Handler stamps a 'ts'. A cache HIT replays it; invalidate evicts → fresh ts.")
    ts1 = getj("/accounts/7")["ts"]
    ts2 = getj("/accounts/7")["ts"]
    out(f"ts #1 {ts1}   ·   ts #2 {ts2}")
    ok("identical → served from cache") if ts1 == ts2 else warn("differ")
    post_json("/cache/invalidate", {"prefix": "GET:/accounts/"})
    time.sleep(0.05)  # let the ms clock tick so a re-run yields a distinct ts
    ts3 = getj("/accounts/7")["ts"]
    out(f"ts #3 {ts3}   (after POST /cache/invalidate)")
    ok("changed → cache evicted, handler re-ran") if ts3 != ts1 else warn("unchanged")
    pause()


def cors_section() -> None:
    banner("CORS — global policy (allow_origins = app.example.com)")
    status, _, _ = http("OPTIONS", "/ping", headers={
        "Origin": "https://app.example.com", "Access-Control-Request-Method": "GET"})
    req("OPTIONS /ping   (preflight from allowed origin)")
    out(f"HTTP {status}  (+ Access-Control-* headers)")
    _, _, headers = http("GET", "/ping", headers={"Origin": "https://app.example.com"})
    req("GET /ping   (with Origin header)")
    out(f"access-control-allow-origin: {headers.get('access-control-allow-origin', '(none)')}")
    pause()


def stage_vars_section() -> None:
    banner("Stage variables — per-function config surfaced on the event")
    req("GET /echo-bun")
    out("stageVariables = " + json.dumps(getj("/echo-bun")["stageVariables"], separators=(",", ":")))
    pause()


def safety_section() -> None:
    banner("On-box safety — per-function caps + an always-on profile")
    sub("echo-python opts into caps; an always-on profile wraps EVERY handler:")
    capturing, caps = False, []
    for line in CFG.read_text().splitlines():
        if line.startswith("[function.echo-python]"):
            capturing = True
        elif capturing and line.startswith("[["):
            break
        elif capturing and (line.startswith("cpu_time_secs") or line.startswith("allowed_paths")):
            caps.append(line)
    print(indent("\n".join(caps)))
    ok("cpu_time_secs → RLIMIT_CPU · allowed_paths → Landlock (Linux) · memory_mb → RLIMIT_AS")
    ok("always-on (every child): RLIMIT_CORE=0, fd/file-size caps, PDEATHSIG, NO_NEW_PRIVS")
    pause()


def authorizers_section() -> None:
    banner("Lambda authorizers (REQUEST type)")
    status, _, _ = post_json("/protected", {"event": "hello"})
    req("POST /protected   (gated by auth-allow)")
    out(f"HTTP {status}   → allowed")
    req("GET /forbidden    (gated by auth-deny)")
    out(f"HTTP {get('/forbidden')[0]}   → denied, handler never ran")
    sub("JWT authorizers (RS256/ES256 vs your IdP's JWKS — Auth0/Cognito/Okta) ship too;")
    sub("see examples/riz.jwt.toml, proven in tests/wave_3_acceptance.rs.")
    pause()


def websocket_section() -> None:
    banner("WebSocket — $connect / $default / $disconnect across 3 runtimes")
    sub("Native WS client (raw socket handshake) — no websocat needed.")
    for path, msg in [("/chat", "hello-bun"), ("/chat-python", "hello-py"), ("/chat-rust", "hello-rs")]:
        reply = ws_roundtrip(path, msg)
        shown = f"{GREEN}{reply}{RESET}" if reply is not None else f"{RED}(no reply){RESET}"
        print(f"  {CYAN}{path:<14}{RESET} send {msg:<10} → {shown}")
    pause()


def hot_reload_section() -> None:
    banner("Handler hot-reload — edit a handler, next request runs new code")
    ST.ping_backup = ST.ping_src.read_bytes()  # cleanup() restores on exit
    req("GET /ping   (before)")
    out(json.dumps(getj("/ping"), separators=(",", ":")))

    sub('editing ping/index.ts:  status "ok" → "hot-reloaded" …')
    edited = ST.ping_backup.decode().replace('status: "ok"', 'status: "hot-reloaded"')
    ST.ping_src.write_text(edited)

    sys.stdout.write(f"  {DIM}waiting for hot-swap {RESET}")
    sys.stdout.flush()
    swapped = False
    for _ in range(20):
        _, body, _ = get("/ping")
        if "hot-reloaded" in body:
            print(f"{GREEN}✓{RESET}")
            swapped = True
            break
        sys.stdout.write(".")
        sys.stdout.flush()
        time.sleep(0.5)
    if not swapped:
        print()
        warn("hot-swap not observed within timeout")

    req(f"GET /ping   (after — same pid {ST.riz.pid}, no restart)")
    out(json.dumps(getj("/ping"), separators=(",", ":")))
    ST.ping_src.write_bytes(ST.ping_backup)
    ST.ping_backup = None
    ok("handler restored to original")
    pause()


def scaffolding_section() -> None:
    banner("Scaffolding & diagnostics")
    r = subprocess.run([str(BIN), "init", "--list"], capture_output=True, text=True, stdin=DEVNULL)
    tpl_rows = []
    for ln in (r.stdout + r.stderr).splitlines():
        cells = re.split(r"\s{2,}", ln.strip())
        if len(cells) >= 3 and ("-http" in cells[0] or "-websocket" in cells[0]):
            tpl_rows.append(cells[:3])
    sub(f"riz init — {len(tpl_rows)} project templates embedded in the binary:")
    render_table(["Template", "Scenario", "Language"], tpl_rows)
    sub("riz doctor — preflight (config, runtimes on PATH, port):")
    r = subprocess.run([str(BIN), "--config", str(CFG), "doctor"], capture_output=True, text=True, stdin=DEVNULL)
    last = (r.stdout + r.stderr).rstrip().splitlines()
    out(last[-1] if last else "(no output)")
    pause()


def final_telemetry() -> None:
    banner("Final telemetry — invocations after the run")
    h = getj("/_riz/health")
    rows = [[f["name"], str(f["invocations"]), "healthy" if f["healthy"] else "DOWN"]
            for f in h["functions"]]
    render_table(["Function", "Calls", "Status"], rows)
    ok(f"uptime {h['uptime_secs']}s · {len(h['functions'])} functions registered")
    print(f"\n{BOLD}{GREEN}✓ demo complete — every capability shown live.{RESET}  Server log: {LOG}")


# ════════════════════════════════ Main ══════════════════════════════════
def main() -> None:
    if not (BIN.exists() and os.access(BIN, os.X_OK)):
        die("build first: cargo build --release")
    if not CFG.is_file():
        die(f"missing {CFG}")

    save_tty()
    try:
        build_wasm()
        boot()
        warm_ollama()
        system_surface()
        mcp_section()
        gateway_section()
        runtimes_section()
        http_shapes_section()
        caching_section()
        cors_section()
        stage_vars_section()
        safety_section()
        authorizers_section()
        websocket_section()
        hot_reload_section()
        scaffolding_section()
        final_telemetry()
    finally:
        cleanup()


if __name__ == "__main__":
    main()
