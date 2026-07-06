# MCP registry submission manifests

This directory holds the manifests and submission instructions for listing riz in
the registries that autonomous agents crawl when resolving "add an MCP server for X."
It implements §3 of the GTM/AEO plan ([`docs/plans/2026-06-10-gtm-and-aeo.md`](../docs/plans/2026-06-10-gtm-and-aeo.md)),
the highest-leverage agent-discovery channel.

## What riz actually is (read this before editing any manifest)

riz is **not** a typical fixed-tool MCP server installed via npm or pip. It is a
self-hosted **runtime binary** (~10 MB, Rust, Apache-2.0; prebuilt for Linux/macOS,
x86_64 + aarch64) that you run on your own machine. The moment it boots, **each
function declared in the user's `riz.toml` is auto-exposed as one MCP tool** at the
Streamable-HTTP endpoint `/_riz/mcp`. Its tool set is therefore **dynamic and
user-defined** — there is no fixed list of tools that riz "ships."

Every registry listing must describe riz on exactly these terms: a local runtime
you run that turns your own HTTP/Lambda handlers into MCP tools.

| Fact | Value |
|---|---|
| Transport | Streamable HTTP (MCP spec **2025-11-25**; negotiates 2024-11-05 / 2025-03-26 / 2025-06-18) |
| Endpoint | `/_riz/mcp` |
| Install | `curl -fsSL https://riz.dev/install \| sh` |
| Connect | `claude mcp add riz-local --transport http http://localhost:3000/_riz/mcp` |
| Repo | https://github.com/24X7/riz |
| Site | https://riz.dev |
| License | Apache-2.0 |
| Reverse-DNS name | `io.github.24X7/riz` |

Shipped capabilities only (no roadmap claims): five runtimes in one binary
(Bun, Node.js, Python, Rust, capability-sandboxed WASM/WASI deny-by-default),
auto-exposure of every `riz.toml` function as an MCP tool, built-in
OpenAI-compatible LLM gateway, JWT/JWKS auth, and no per-request cold start
(warm pre-spawned pool per function).

## Files in this directory

- `../server.json` (repo root) — the official Model Context Protocol registry entry.
  It lives at the repo root because that is where the `mcp-publisher` CLI and the
  registry's GitHub-auth flow expect to find it.
- `README.md` (this file) — the registry index and per-registry submission guide.

There is intentionally **no `smithery.yaml`** in this directory. See the Smithery row
below for the reason.

## Registries riz fits

### Official MCP registry — `registry.modelcontextprotocol.io` — FITS

- **Manifest:** [`../server.json`](../server.json), validated against the
  `2025-12-11` `server.schema.json`.
- **Why it fits:** the schema's `remotes` array models exactly what riz is — an MCP
  server reachable over a Streamable-HTTP URL. We use a `remotes` entry of type
  `streamable-http` pointing at the local endpoint (`http://localhost:{port}/_riz/mcp`,
  `port` defaulting to 3000), and we use the `_meta.io.modelcontextprotocol.registry/publisher-provided`
  block to state plainly that this is a self-hosted runtime whose tools are dynamic
  and user-defined, with the install and connect commands.
- **Why not a `packages` entry:** `packages` registry types are `npm`, `pypi`,
  `cargo`, `nuget`, `oci`, `mcpb`. riz is distributed today as prebuilt binaries via
  a `curl | sh` installer and GitHub Releases — none of those package types actually
  fits yet. (If/when riz ships to `crates.io` or as an `mcpb` bundle — both are
  on the GTM roadmap — add a corresponding `packages` entry then.)
- **Submission action (CLI publish):**
  1. Install the publisher: `brew install mcp-publisher` (or download from
     `github.com/modelcontextprotocol/registry` releases).
  2. From the repo root: `mcp-publisher login github` (authenticates the
     `io.github.24X7/*` namespace against the GitHub repo).
  3. `mcp-publisher publish` — reads `./server.json` and submits it.
- **What's needed:** push access to `github.com/24X7/riz` (the GitHub OAuth flow
  proves namespace ownership for `io.github.24X7/riz`). No web form.

### Smithery — `smithery.ai` — PARTIAL FIT (external URL only; no repo manifest)

- **Why there is no `smithery.yaml`:** Smithery's in-repo `smithery.yaml` (with
  `runtime: typescript` or `runtime: container` + `build` + `startCommand`) is for
  servers that **Smithery builds and hosts on its own infrastructure** — it compiles
  a JS module or Docker image and runs it for users. riz is a self-hosted binary that
  runs on the *user's* machine and exposes the *user's own* dynamic functions; it is
  not a server we hand to Smithery to deploy and host. Committing a `smithery.yaml`
  here would misrepresent riz as a Smithery-deployable, fixed-tool server. So we don't.
- **What does fit:** Smithery's **external / URL-based** listing path, for servers the
  user self-hosts. This requires no file in the repo.
- **Submission action:** either
  - publish via the CLI against the running endpoint:
    `smithery mcp publish "http://localhost:3000/_riz/mcp" -n 24X7/riz`, or
  - submit the server URL through the web form at `https://smithery.ai/new`.
  In both cases the listing must carry the same framing: a self-hosted runtime
  whose tools are dynamic and user-defined.
- **What's needed:** a Smithery account.

### Other directories (web-form / PR — no manifest file needed)

These accept a repo URL + description through a form or a PR rather than a manifest
committed here. Use the framing above and the `when_to_use` bullets from §3 of the
GTM plan.

| Directory | Submission action | What's needed |
|---|---|---|
| **mcp.so** | Web form at mcp.so with repo URL, description, transport (`http`), tags | Form submission |
| **Glama** (`glama.ai/mcp`) | Web form or GitHub PR per their submit instructions | Form / PR |
| **PulseMCP** (`pulsemcp.com`) | Web form with repo URL + description | Form submission |
| **Awesome MCP Servers** (`appcypher/awesome-mcp-servers`) | PR adding riz under "Self-hosted / Infrastructure" | GitHub PR |
| **Awesome MCP Servers** (`punkpeye/awesome-mcp-servers`) | PR adding riz | GitHub PR |
| **Cline MCP marketplace** (`github.com/cline/cline`) | PR to their marketplace list | GitHub PR |

## Schema sources fetched (for provenance)

- Official `server.json` reference and examples:
  `https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/generic-server-json.md`
- Official `server.json` JSON Schema (authoritative for required fields / constraints):
  `https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`
  — top-level required: `name`, `description`, `version`; `description` maxLength 100;
  `name` pattern `^[a-zA-Z0-9.-]+/[a-zA-Z0-9._-]+$`; `remotes[].type` ∈
  {`streamable-http`, `sse`}.
- Smithery docs (publish + project config): `https://smithery.ai/docs`,
  `https://smithery.ai/docs/build/publish.md`, and the `smithery.yaml` reference under
  `smithery.ai/docs/build/project-config`.

## Listing rules

Only shipped capabilities are described. No fixed tool list is fabricated (riz's tools
are user-defined at runtime). No schema field is invented — every field in
`server.json` comes from the fetched `2025-12-11` schema. Where riz does not cleanly
fit a registry's required shape (Smithery's deploy-and-host model; the official
registry's `packages` types), that is stated plainly rather than forced.
