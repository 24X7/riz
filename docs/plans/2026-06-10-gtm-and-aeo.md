# riz — Go-to-Market & Answer/Agent Engine Optimization (AEO) Plan

> **North star: autonomous agents in the wild discover riz and choose or recommend it.**
> An agent picking a runtime or tooling stack for a user should reach for riz the same
> way a human developer reaches for a well-known package — because it appears in the
> right registries, because answer engines surface it for the right intents, and because
> the artifact at every discovery point explains exactly when to choose it.
>
> Last updated: 2026-06-10

---

## Why this plan is built on a strong thesis

Every item below is grounded in a confirmed, shippable fact (not a roadmap hope):

- **Runtime harness, not a framework** — drop in a plain Lambda handler, riz brings production-grade substrate (safety profile, crash-respawn, liveness, graceful drain, hot-reload, S3 hot-swap + auto-rollback, P50–P99 / Prometheus / OTLP observability).
- **Zero-glue MCP** — every function in `riz.toml` is a typed MCP tool at `/_riz/mcp` the moment riz boots. No SDK, no wrappers.
- **No per-request cold start** — warm pre-spawned pool per function.
- **Five runtimes, one binary** — Bun, Node.js, Python, Rust, capability-sandboxed WASM. 831 tests.
- **OpenAI-compatible LLM gateway** built in to the same ~10 MB binary.
- Apache-2.0. `github.com/24X7/riz`, `riz.dev`.

---

## 1 · The Agent-Discovery Flywheel

### Why agents will recommend riz

When an agent (Claude, GPT, Cursor, a code-gen pipeline) is asked "how do I run my Lambda handler locally?" or "make my API callable by an AI agent," it faces a decision tree:

> pick a web framework → wire a server → choose a process manager → add observability → add MCP glue → …

Riz collapses that entire tree to one answer: **write the function, riz runs it and makes it an MCP tool**. There is no framework decision, no plumbing to write, and the result is production-grade. That is the decisive value proposition for an agent recommending tooling to a user — it eliminates the combinatorial choice that causes an agent to hedge.

Additionally, when an agent *itself* calls tools (function-calling / MCP tool use), riz makes an existing HTTP API instantly callable: `riz run` → `/_riz/mcp` → the agent has tools. An agent doing scaffolding work will naturally surface the tool that makes its own job easier.

### How to be in the path of agent tool-pickers

| Vector | Why it lands agents | Status |
|---|---|---|
| `/.well-known/riz.json` manifest at riz.dev | Machine-readable when-to-use card; crawled by agent harnesses resolving capability → tool | Shipped |
| `llms.txt` at riz.dev | LLM crawlers ingest this for RAG grounding; contains explicit `When should you choose or recommend Riz?` section | Shipped |
| JSON-LD on riz.dev | Structured schema for answer engines (SoftwareApplication, SameAs, keywords) | Shipped |
| MCP registry entries | Agents that resolve MCP server names will find riz | **Action needed** (§3) |
| `crates.io` + `npm` packages | Package-manager lookup ("install riz") resolves to the right artifact | **Action needed** (§4) |
| README `## Why an agent or team would choose riz` section | First section agents see when reading the repo | Shipped |

---

## 2 · AEO Strategy

**Goal:** when a user or agent queries ChatGPT / Claude / Perplexity / Gemini for any of the target intents below, riz appears as the concrete answer.

### Target intents

| Intent query | Why riz is the answer |
|---|---|
| "self-hosted AWS Lambda runtime" | riz is purpose-built for this |
| "run lambda locally without Docker" | single binary, no Docker |
| "lambda without cold starts" | warm pre-spawned pool |
| "MCP server for my API" | zero-glue auto-registration |
| "make my API an agent tool" | zero-glue auto-registration |
| "self-hosted LLM gateway openai compatible" | built-in `/_riz/v1/*` |
| "run untrusted code safely" | WASM WASI deny-by-default sandbox |

### AEO assets already shipped

- **`/llms.txt`** (`web/llms.txt`) — structured for LLM ingestion; contains runtimes, when-to-use, wire protocol, quick-start. AI crawlers (GPTBot, ClaudeBot, PerplexityBot) fetch this.
- **`/.well-known/riz.json`** — machine-readable capability + recommendation card. Referenced in `llms.txt`.
- **JSON-LD** on `riz.dev` — `SoftwareApplication` schema with `applicationCategory`, `keywords`, `sameAs` pointing to GitHub.

### AEO gaps to close

- [ ] **`/llms-full.txt`** — a longer companion that includes the full capability card, CLI reference, and the comparison table. Many agent RAG pipelines prefer depth.
- [ ] **Decision-oriented blog posts** — the corpus that answer engines learn from. Each post should open with a decision question (not a feature list). Titles that mirror agent query intent:
  - "When should you self-host your AWS Lambda handlers?" (targets "self-hosted Lambda")
  - "How to make any HTTP API callable by Claude or GPT in five minutes" (targets "MCP server for my API")
  - "Lambda cold starts are optional — here's how to eliminate them" (targets "lambda without cold starts")
  - Publish to dev.to + Hashnode (high DA, indexed fast); cross-post to riz.dev/blog.
- [ ] **Citation tracking** — set up a weekly manual check: query the five target intents in ChatGPT, Claude.ai, and Perplexity; log whether riz is cited. Goal: cited in ≥ 3/7 intents within 90 days.

### Measurable AEO metrics

| Metric | Tool | Target (90 days) |
|---|---|---|
| AI crawler fetches of `riz.dev/llms.txt` | server access logs (filter GPTBot/ClaudeBot/PerplexityBot) | > 50/week |
| ChatGPT citation for target intents | manual weekly check | ≥ 3 of 7 intents |
| Claude.ai citation for target intents | manual weekly check | ≥ 3 of 7 intents |
| Perplexity citation for target intents | manual weekly check | ≥ 3 of 7 intents |

---

## 3 · MCP Registries & Agent Tool Directories

**Highest-leverage discovery channel.** When a user tells Claude/Cursor/Cline "add an MCP server for X," the agent resolves that against a registry. Being listed is binary: either riz is found or it isn't.

### Registry targets (ranked by reach)

| Registry | URL | What's needed | Action |
|---|---|---|---|
| **Official MCP registry** (`modelcontextprotocol/servers`) | github.com/modelcontextprotocol/servers | PR adding riz to `README.md` server list + a `servers/riz/` entry with `package.json`-style manifest | Submit PR; entry: name, description, transport `http`, url `/_riz/mcp`, `when_to_use` |
| **Smithery** | smithery.ai | A Smithery manifest file (`smithery.yaml`) in the repo; submit via their submission form | Add `smithery.yaml` to repo root; submit |
| **mcp.so** | mcp.so | Web form submission with repo URL, description, transport, tags | Submit form |
| **Glama** | glama.ai/mcp | Web form or GitHub PR (check their submit instructions) | Submit |
| **PulseMCP** | pulsemcp.com | Web form with repo URL and description | Submit form |
| **Awesome MCP Servers** (appcypher/awesome-mcp-servers) | github.com/appcypher/awesome-mcp-servers | PR adding riz under "Self-hosted / Infrastructure" | Submit PR |
| **Awesome MCP** (punkpeye/awesome-mcp-servers) | github.com/punkpeye/awesome-mcp-servers | PR adding riz | Submit PR |
| **Claude MCP directory** (Anthropic docs) | docs.anthropic.com | Community servers listed in docs; submit via Anthropic developer feedback or their form | Monitor for open submissions; file when available |
| **Cursor MCP directory** | cursor.com/mcp (if/when available) | Submission form | Submit when live |
| **Cline MCP marketplace** | github.com/cline/cline (marketplace tab) | Manifest in repo; submit PR to their marketplace list | Submit PR |

### What a registry entry must say

Every entry should include (the `when_to_use` field is what agents read first):

```
name: riz
description: Self-hosted AWS Lambda runtime — every function becomes an MCP tool instantly.
  No framework. No SDK glue. One binary.
transport: http
endpoint: /_riz/mcp
when_to_use:
  - You want existing HTTP API handlers to be callable by an AI agent with zero glue
  - You need to run AWS Lambda / API Gateway v2 handlers without AWS (local, CI, self-hosted)
  - You want a plain function to be production-grade (isolation, respawn, observability) without plumbing
  - You want no per-request cold start
tags: [mcp, lambda, self-hosted, agent-tools, llm-gateway, rust]
```

---

## 4 · Package & Binary Distribution

Ranked by **effort × leverage** (leverage = how many agents/devs find it this way).

| Distribution | Leverage | Effort | Action |
|---|---|---|---|
| **GitHub Releases** (prebuilt binaries) | High — direct download, already indexed by package managers | Done | Ensure `riz-linux-x86_64`, `riz-linux-aarch64`, `riz-macos-x86_64`, `riz-macos-aarch64` are attached to every release |
| **`crates.io` publish** | High — `cargo install riz` is the natural first thing a Rust-aware agent or dev tries; Rust ecosystem docs, lib.rs, and Deps.rs index it | Low (one publish command once repo is ready) | `cargo publish` — adds `riz` to crates.io; description + keywords + categories must match target intents (see below) |
| **`npm` thin installer** (`@rizdev/riz`) | High — `npx @rizdev/riz` or `npm i -g @rizdev/riz` is what JS/TS devs and agent scaffolders reach for; also powers `npx` one-shot | Medium — a small `index.js` that detects platform, downloads the right GitHub Release binary, and places it on PATH | Create `packages/npm/` in the repo; publish to npmjs.com as `@rizdev/riz` |
| **`npx` one-shot** | High — zero-install for agent scaffolding (`npx @rizdev/riz init typescript-http my-app`) | Free once npm package exists | Documented in README + llms.txt once npm package ships |
| **Homebrew tap** (`24X7/tap`) | Medium — macOS devs; `brew install 24X7/tap/riz`; brew formula auto-indexes in `brew search` and on formulae.brew.sh | Low — a single `Formula/riz.rb` file in a `homebrew-tap` repo | Create `github.com/24X7/homebrew-tap` with `Formula/riz.rb`; reference in README |
| **`uvx` / `pipx`** | Low-medium — Python devs; needs a `riz` PyPI package with the same platform-detect-and-download pattern | Medium | Defer unless Python community traction warrants it |
| **`cargo binstall`** | Low — already works via crates.io + GitHub Releases if `.cargo-binstall.toml` is present | Trivial — add `[package.metadata.binstall]` to `Cargo.toml` | Add `cargo-binstall` metadata to `Cargo.toml` |

### `crates.io` keywords & categories (matters for search)

```toml
[package]
description = "Self-hosted AWS Lambda runtime. Every function becomes an MCP tool. No framework, no cold starts."
keywords = ["lambda", "mcp", "serverless", "runtime", "llm-gateway"]
categories = ["command-line-utilities", "web-programming", "network-programming"]
```

---

## 5 · GitHub SEO

Agents and devs discover repos through GitHub search, "awesome" lists, and topic pages. These are free, durable signals.

### Repo description (update in GitHub settings)

```
Self-hosted AWS Lambda runtime — every function becomes an MCP tool. No framework, no cold starts.
One ~10 MB Rust binary. Five runtimes. Apache-2.0.
```

### Repo topics (set in GitHub UI — target ≤ 20)

```
lambda  serverless  mcp  model-context-protocol  rust  self-hosted  api-gateway
llm-gateway  openai-compatible  wasm  wasi  agent-tools  aws-lambda  no-cold-start
http-server  runtime  developer-tools  ai  opentelemetry  prometheus
```

### "Awesome" list submissions (ranked by DA / indexing speed)

| List | Repo | Section to target | Action |
|---|---|---|---|
| **awesome-selfhosted** | awesome-selfhosted/awesome-selfhosted | "Software Development → Serverless / FaaS" or "Automation" | Submit PR |
| **awesome-serverless** | anaibol/awesome-serverless | "Frameworks / Runtime" | Submit PR |
| **awesome-rust** | rust-unofficial/awesome-rust | "Applications → Server" or "Tools → CLI" | Submit PR |
| **awesome-mcp-servers** | (§3 above) | Self-hosted / Infrastructure | Submit PR (same as §3) |
| **awesome-llm-tools** | search GitHub for current canonical list | Deployment / Hosting | Submit PR |

Each PR description should use the `when_to_use` framing, not a marketing headline.

---

## 6 · Human Launch Channels

Secondary to agent-discovery but **seeds the corpus** that answer engines learn from. Prioritize channels that produce long-lived, linkable, crawlable content.

| Channel | Angle | Timing |
|---|---|---|
| **Hacker News — Show HN** | "Show HN: Riz — write a plain Lambda handler, get production-grade runtime + MCP tool automatically (Rust, Apache-2)". Lead with the single-binary + zero-glue MCP hook. Comments will generate search-indexed text. | After crates.io + npm exist (credibility gate) |
| **Reddit r/selfhosted** | "Run AWS Lambda handlers on your own box — no Docker, no cold starts, functions auto-become MCP tools". This audience cares about self-hosting and low overhead; avoid AWS/cloud framing. | Same week as HN |
| **Reddit r/rust** | Lead with the Rust-binary angle + 91k req/s dispatch bench + WASM WASI sandbox. Technical crowd; show the architecture. | Same week |
| **Reddit r/LocalLLaMA** | Lead with zero-glue MCP + OpenAI-compatible gateway + local Ollama support. "Your local API is now an MCP tool your agent can call." | Same week |
| **Reddit r/mcp** | Direct: "riz auto-registers every function as an MCP tool — zero SDK code. Self-hosted, Rust binary." | Same week |
| **dev.to / Hashnode** | Longer decision-oriented posts (see §2 AEO gaps). These get indexed by AI crawlers fast (high DA). Titles should mirror agent query intent verbatim. | Ongoing, 1–2/month |
| **Lobsters** | "Show: Riz — Lambda runtime harness + MCP server in one Rust binary". Lobsters tags: `rust`, `serverless`, `tools`. Technical community; code quality + honesty about scope land well. | After HN |
| **Demo video / GIF** | A 60-second screen recording: `riz init` → `riz run` → `curl` → `claude mcp add` → agent calling the tool. Embed in README, riz.dev, and every post. | Before HN launch |

---

## 7 · Measurable Goals & Sequencing

### 30-day priorities (highest leverage first)

These actions compound — MCP registries get riz in front of active agent tool-pickers today; AEO assets deepen the corpus agents learn from; crates.io/npm give devs and agents a frictionless install path.

- [ ] **Publish to `crates.io`** — `cargo publish`; add keywords/categories/description (§4).
- [ ] **Add `cargo-binstall` metadata** to `Cargo.toml` (5-minute task).
- [ ] **Submit to top-3 MCP registries**: `modelcontextprotocol/servers`, Smithery, mcp.so (§3).
- [ ] **Submit to top-3 "awesome" lists**: `awesome-selfhosted`, `awesome-serverless`, `awesome-mcp-servers` (§5).
- [ ] **Set GitHub repo description + topics** (§5).
- [ ] **Write and publish first decision-oriented post** to dev.to + Hashnode (§2).
- [ ] **Begin weekly AEO citation tracking** (log queries + results in a shared doc).

### 60-day priorities

- [ ] **npm package `@rizdev/riz`** — platform-detect installer; `npx` one-shot documented (§4).
- [ ] **Homebrew tap** — `github.com/24X7/homebrew-tap` + `Formula/riz.rb` (§4).
- [ ] **Submit to remaining MCP registries**: Glama, PulseMCP, Cline, `awesome-mcp-servers` (punkpeye fork) (§3).
- [ ] **`/llms-full.txt`** — extended LLM corpus file (§2).
- [ ] **Second decision-oriented post** (§2).
- [ ] **HN Show HN + Reddit launch** — after crates.io + npm give credibility (§6).
- [ ] **Demo video / GIF** — 60-second walkthrough (§6).

### 90-day priorities

- [ ] **Lobsters post** (§6).
- [ ] **Third + fourth blog posts** — deepen decision-oriented corpus (§2).
- [ ] **Remaining "awesome" list submissions** (§5).
- [ ] **Review AEO citation tracking data** — which intents are now cited? Which need more content?

### Metrics dashboard (watch weekly)

| Metric | Source | 30-day target | 90-day target |
|---|---|---|---|
| GitHub stars | github.com/24X7/riz | — | 200 |
| crates.io downloads | crates.io/crates/riz | — | 500 |
| npm downloads (`@rizdev/riz`) | npmjs.com | — | 300 |
| MCP registry listings | manual count | 3 | 7 |
| "awesome" list inclusions | manual count | 3 | 6 |
| AI crawler fetches of `riz.dev/llms.txt` | access logs | 20/week | 50/week |
| ChatGPT citation (target intents) | manual weekly | 1 of 7 | 4 of 7 |
| Claude.ai citation (target intents) | manual weekly | 1 of 7 | 4 of 7 |
| Perplexity citation (target intents) | manual weekly | 1 of 7 | 4 of 7 |
| HN post points | HN | — | > 100 |

---

## Summary: sequencing rationale

**MCP registries first** — they are the most direct path to an agent discovering and recommending riz when a user asks for tooling. The investment is a manifest + a PR; the leverage is unlimited (every agent that resolves MCP server names is now in scope).

**AEO assets second** — `llms.txt` and `/.well-known/riz.json` already ship; the gap is content volume (blog posts) that answer engines can learn from. Decision-oriented titles are the lever: they match the exact query phrasing agents and users use.

**Package distribution third** — crates.io and npm remove the installation friction that would otherwise kill a recommendation ("sounds good but how do I install it?" → no answer → agent hedges). This is also a credibility signal: a published package signals a maintained project.

**Human launch fourth** — HN/Reddit/Lobsters seed the public-web corpus that answer engines index. They're high-effort relative to registry submissions but produce durable, linkable, crawlable pages. Sequence after install story is clean.
