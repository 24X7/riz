# riz

> **Self-hosted AWS Lambda runtime where every function becomes a typed MCP tool.**

riz is a **runtime harness, not a framework**. Write a plain AWS-Lambda-shaped
HTTP/WebSocket handler — no web framework to pick — and riz runs it on your own
box, *unmodified*, and makes it production-grade for free: warm process pools (no
per-request cold start), a layered worker sandbox, supervised crash-respawn,
graceful drain with a Kubernetes-style readiness probe, hot-reload, and
P50–P99 + Prometheus observability. Every function auto-becomes a typed **MCP
tool** an agent can call the moment riz boots, and a built-in
**OpenAI-compatible LLM gateway** routes, governs, and costs the model calls
your handlers make. One self-contained Rust binary, ~11 MB download. Apache-2.0.

**📖 Full docs, comparisons, and the agent layer live at [riz.dev](https://riz.dev).**
This README is the short version.

```bash
curl -fsSL https://riz.dev/install | sh          # prebuilt binary (macOS/Linux)
riz new typescript-bun my-app && cd my-app && riz run
# → curl 'localhost:3000/hello?name=alice'  →  {"message":"hello, alice", ...}
```

[Website](https://riz.dev) · [Compare](https://riz.dev/compare.html) · [Docs](https://riz.dev/docs.html) · [Agents](https://riz.dev/agents.html) · [Releases](https://github.com/24X7/riz/releases) · [![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](./LICENSE)

---

## Three products, one binary

The combination is the point — no Lambda emulator ships an MCP server or an LLM
gateway, and no AI gateway runs your Lambda code.

| | What it gives you |
|---|---|
| ⚡ **A Lambda runtime** | Drop in AWS HTTP API v2 + WebSocket handlers **unchanged** across **six runtimes** — Bun, Node.js, Python, Rust, Go, and capability-sandboxed WASM. Rust and Go run *stock, unmodified* official AWS Lambda binaries (`lambda_runtime`, `aws-lambda-go`) via the real Lambda Runtime API — no riz library. One warm pool per function, no container per request, no cloud bill. |
| 🤖 **An MCP server** | Every function in `riz.toml` becomes an agent-callable tool at `/_riz/mcp` (spec **2025-11-25**) — typed input schemas, SSE, progress, sessions. Point Claude / Cursor at it with **zero SDK code**. |
| 💸 **An LLM gateway** | An OpenAI-compatible endpoint at `/_riz/v1/*`. Route across **OpenAI / Anthropic / Ollama** with fallback, stream over SSE, and cap spend with budgets + per-provider cost telemetry. |

## Quick start

```bash
# Install — prebuilt binary for macOS (Apple Silicon) and Linux (x86_64/arm64).
curl -fsSL https://riz.dev/install | sh
# From source instead: cargo install --git https://github.com/24X7/riz

# A runtime needs its toolchain on PATH: bun (TS/JS), python3, node, go,
# or a compiled Rust/Go binary. WASM handlers need the wasm32-wasip1 target.
curl -fsSL https://bun.sh/install | bash

riz new typescript-bun my-app      # scaffold (see `riz new --list` for templates)
cd my-app
riz run                            # headless JSON logs; add --dev for the live TUI

curl 'http://localhost:3000/hello?name=alice'
# → {"message":"hello, alice","method":"GET","functionName":"hello","remainingMs":...}
```

Point an agent at it: `claude mcp add riz --transport http http://localhost:3000/_riz/mcp`.

## What it does

- **Runs your handlers unmodified** — AWS API Gateway v2 HTTP + WebSocket payloads
  via `aws_lambda_events`; handlers move between AWS and riz both directions.
- **No per-request cold start** — a warm pre-spawned process pool per function;
  cold starts only at boot, respawn, or hot-swap.
- **Typed MCP tools, zero glue** — path params become typed+required, `[function.x.mcp]`
  declares query/body schemas; `tools/call` validates and names the bad parameter.
- **Layered worker sandbox** — every worker runs under always-on `RLIMIT_*` +
  `PR_SET_PDEATHSIG` + `NO_NEW_PRIVS` + a **seccomp-BPF syscall blocklist**
  (ptrace, mount, kexec, kernel-module/eBPF loading, the keyring, …); opt-in
  per-function memory / CPU / (Linux) Landlock filesystem caps.
- **Readiness + metrics** — `/_riz/health` (liveness) and `/_riz/ready` (sheds
  traffic the instant a drain starts) for orchestrators, plus a Prometheus
  `/_riz/metrics` endpoint with **saturation** (concurrency utilization,
  load-shed), worker reliability, and latency.
- **Capability-sandboxed WASM** — `runtime = "wasm"` runs a `wasm32-wasip1` module
  under wasmtime (deny-by-default fs/net), plus a resource broker and fail-closed
  `.wasm` guards for running untrusted / LLM-generated code.
- **Lifecycle built in** — supervised respawn, liveness, graceful 30s drain,
  hot-reload, and S3 hot-swap deploys with health-check auto-rollback.
- **Colocate your site** — point `[static]` at a dir to serve an SPA + the
  instance's own `llms.txt` / `.well-known/riz.json` on the same origin, no CORS.

See **[riz.dev/docs.html](https://riz.dev/docs.html)** for the full reference and
**[riz.dev/compare.html](https://riz.dev/compare.html)** for how it stacks up
against AWS Lambda and web frameworks.

## How it scales

riz handlers are HTTP API services with no per-request cold start, so you run riz
**always-on**: wrap the binary in a container, deploy it on a managed HTTP
container service, keep a **warm floor** (min one instance), and let the platform
autoscale *up* on load — **Google Cloud Run** (`min-instances ≥ 1`; up to 1000
concurrent/instance), **AWS ECS Express Mode** (App Runner's successor — a Fargate service with a
URL + ALB + SSL, autoscaled), or **Azure Container Apps**
(`minReplicas ≥ 1`, KEDA on HTTP concurrency). Each instance serves many
concurrent requests from its warm pools, so even the first request after a spike
hits a warm instance — never a container cold start. (Scale-to-zero is available
but reintroduces that cold start; keep the floor on the hot path. More control:
ECS on Fargate, k8s, or a VM behind a load balancer.)

**WebSockets across replicas:** connections and the `@connections` push registry
are per-instance (in-memory), so running N replicas behind a load balancer
**requires sticky sessions** (session affinity — Cloud Run `--session-affinity`,
ALB target-group stickiness, nginx `ip_hash`). Affinity keeps a client's socket
and the pushes its own requests trigger on one instance; cross-client broadcast
across replicas needs a shared backplane, which is a roadmap item, not in the
binary today. Details on [riz.dev/compare.html](https://riz.dev/compare.html).

## What riz is *not*

Out of scope by decision, not omission:

- **Not a full AWS emulator** — HTTP/WS Lambda only. No SQS/SNS/S3/EventBridge/
  DynamoDB-stream triggers, no Step Functions. Use real AWS or LocalStack.
- **Not an IAM / credential emulator** — a handler that calls the AWS SDK brings
  its own credentials, same as anywhere.
- **Not an edge/CDN platform** — it's a runtime you self-host. No Windows.

## Tested — and safety-gated

**1,100+ tests** (`cargo nextest run`) — unit, integration, a cross-runtime
parity matrix, and an end-to-end smoke harness that boots the real binary
against every example across all six runtimes. Every public capability on
[riz.dev](https://riz.dev) is pinned to a passing test via the claims registry,
so the marketing and the code can't drift. CI runs the full suite on every PR.

The runtime also holds to **[NASA's Power of 10](docs/SAFETY.md)**, adapted to
Rust: **253 latent panic / overflow / unbounded-growth sites in the binary
driven to zero** and kept there by CI lints — no reachable panic on the request
path, bounded queues with backpressure, checked arithmetic (overflow panics
loudly in release rather than wrapping), and every `unsafe` block justified with
a proof. The security posture and roadmap toward larger-scale production are in
[docs/SAFETY.md](docs/SAFETY.md), [docs/PRODUCTION-READINESS.md](docs/PRODUCTION-READINESS.md),
and [docs/METRICS.md](docs/METRICS.md).

```bash
cargo nextest run --workspace        # the whole suite
bash examples/smoke-all.sh           # the end-to-end harness, with a ✓/✗ report
```

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). The hard rule: `cargo nextest run`,
never `cargo test`. Before pushing: `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets -- -D warnings`.

## License

[Apache-2.0](./LICENSE).
