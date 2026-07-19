# WASM resource-broker design — host-mediated capability access (Phase 5)

Status: **ROADMAP** (design only — nothing in this document is shipped).
Branch: `claims-truth-ai-substrate`.
Last updated: 2026-06-10.

## Goal

Let a WASI guest reach external resources — Postgres (Neon / Supabase / any
PG-wire), object storage (S3), KV/Dynamo, allow-listed HTTP — **without
weakening the deny-by-default sandbox or the host's resiliency.** The guest
never opens a socket. Every external interaction is a *brokered call*: the guest
asks the host, the host decides (against an explicit per-function capability
grant in `riz.toml`), performs the I/O on its own connection pools under strict
limits, and hands back bytes.

One sentence frames the whole design: **the host owns the blast radius.** A
guest can request work; it can never exhaust, stall, or crash the host, and it
can only touch resources an operator explicitly granted it.

This composes with — does not replace — the shipped defenses: wasmtime's WASI
sandbox (no ambient fs/net), Landlock filesystem allow-lists, and the rlimits
profile (`RLIMIT_CORE=0`, memory/CPU caps). The broker adds the *network and
data* dimension those don't cover.

## Why this shape

riz is one lean Rust binary that already owns connection lifecycles for the LLM
gateway and already runs guests as capability-sandboxed children. The broker
reuses that posture: guests stay pure compute; the host — which already has
tokio, TLS, and pools — is the only thing that talks to the outside world. We do
not hand the guest a socket and a firewall to misconfigure; we hand it a
narrow, audited verb set.

## Threat / resiliency model

The sandbox guarantees a guest **cannot** open arbitrary sockets, resolve DNS,
or read the filesystem beyond its preopens. The broker must preserve that
guarantee while adding controlled egress. Threats and their containment:

| Threat | Containment |
| --- | --- |
| Guest reaches a resource it was never granted | **Deny-by-default allow-lists.** A capability does nothing unless `[function.x.capabilities]` names it. No grant → the import returns `Denied`. |
| Guest exfiltrates to an arbitrary host via `http_fetch` | **Host + scheme allow-list per grant.** Only listed origins; the host resolves and dials, the guest never sees an IP. |
| Guest stalls the host with a slow/hung backend | **Per-call timeout** (grant-level, capped by the function `timeout_ms`). The host races the I/O against a deadline and returns `Timeout`; the guest invocation is unaffected structurally. |
| Guest floods a backend / saturates the pool | **Concurrency caps** (max in-flight brokered calls per function and per pool) + **rate limits** (token bucket per capability). Excess returns `Throttled`, never blocks the host loop. |
| Guest pulls a multi-GB row / pushes a huge object | **Payload-size limits** on request and response, enforced *before* buffering. Over-limit returns `TooLarge`. |
| Guest pins host memory by streaming forever | Responses are size-capped and, where the backend supports it, streamed in bounded chunks the host copies into guest memory under a ceiling. |
| A backend credential leaks into guest memory | **Credentials never cross the boundary.** Connection strings, AWS keys, and bearer tokens live host-side, keyed by grant name. The guest references a *named* capability, not a secret. |
| One guest's misbehavior degrades another function | Pools and limit buckets are **per function**; the broker scheduler is isolated from the request event loop (same isolation principle as the telemetry process). |

Every brokered call is also an **audit point**: capability name, backend, byte
counts, latency, and outcome are emitted on the existing telemetry pipeline
(best-effort, never on the hot path).

## One brokered interface, many backends

The guest sees a **small, stable capability API** — a handful of verbs. Behind
each verb the host fans out to whatever backend the named grant points at. The
guest does not know or care whether `pg_query` lands on Neon, Supabase, or a
self-hosted Postgres.

Capability verbs (v-set; not all v1 — see Phasing):

| Verb | Shape | Backend(s) |
| --- | --- | --- |
| `pg_query(grant, sql, params) -> rows` | parameterized query against a named PG pool | **Postgres wire** → Neon, Supabase, RDS, any PG |
| `kv_get(grant, key) -> bytes?` / `kv_put(grant, key, bytes)` | namespaced KV | DynamoDB (single-table) or a PG-backed KV |
| `s3_get(grant, key) -> bytes` / `s3_put(grant, key, bytes)` | object under a granted bucket+prefix | S3 via the host AWS SDK |
| `http_fetch(grant, req) -> resp` | allow-listed outbound HTTP | host `reqwest` client, origin allow-list |

**Postgres-wire is the keystone.** A single `pg_query` capability, backed by the
host's PG pool, covers **Neon** and **Supabase** with zero new code: both speak
standard Postgres wire over TLS. Neon is a connection string to a serverless PG
endpoint; Supabase is a connection string to its managed PG (the host talks
straight to the database, bypassing PostgREST). The *only* per-provider concern
is the DSN and pooler endpoint, which is operator config — so "support Neon" and
"support Supabase" are config rows, not features. **S3 and DynamoDB** ride the
host's existing AWS SDK + credential chain; the guest gets `s3_*` / `kv_*` verbs,
the host gets IAM.

This is the leverage: **one capability verb per resource class, many backends
per verb.** The guest surface stays tiny; the host absorbs backend variety.

## Interface-model options & recommendation

Three ways to expose the verbs to the guest:

1. **WASI Preview 2 component-model interfaces (WIT).** Define the broker as a
   typed WIT world the guest imports; wasmtime synthesizes typed bindings.
   - *Pro:* typed, versionable, idiomatic to where WASI is heading; rich
     value types (records, results, streams) without hand-rolled encoding.
   - *Con:* requires moving guests from `wasm32-wasip1` (preview1, what riz ships
     today) to the component model + `wasm32-wasip2` toolchains; bindings churn;
     larger surface to stabilize. Premature for v1.

2. **Custom host functions (preview1 host imports).** Register native host
   functions in the wasmtime `Linker` (e.g. module `riz:broker`) that the guest
   imports directly. Arguments are scalars + guest-memory pointers/lengths;
   complex values are JSON or a compact binary framing in linear memory.
   - *Pro:* works **today** on `wasm32-wasip1` with the exact runtime riz already
     runs; full host control over limits, pooling, and audit at the call site;
     no toolchain migration for guest authors.
   - *Con:* manual memory marshalling; the ABI is ours to version.

3. **Syscall-style capability API (single dispatch verb).** One host import,
   `broker_call(verb_id, request_ptr, request_len, response_ptr_out)`, with a
   self-describing request frame (the verb is a field). Backends register behind
   the dispatcher host-side.
   - *Pro:* one ABI seam to stabilize and audit; adding a verb is host-side only,
     no new guest import; trivial to gate, meter, and log uniformly.
   - *Con:* less self-documenting than named imports; request framing must be
     disciplined.

**Recommendation: ship v1 on (2) custom host functions, structured internally as
(3) a single dispatcher.** Concretely: a thin set of named guest imports
(`pg_query`, `s3_get`, …) that the host implements by funneling into one internal
`broker_call` dispatcher where *all* limits, allow-list checks, metering, and
audit live in one place. This gives guest authors readable, typed-ish imports
**and** gives the host a single choke point for resiliency — without forcing the
preview2/component-model migration. We keep WIT/preview2 (option 1) as the
**v2 evolution**: once the verb set is stable and the component-model toolchain
is boring, we publish it as a WIT world and the dispatcher becomes the host
implementation behind it. The wire framing for request/response is JSON in
linear memory for v1 (mirrors the stdin/stdout JSON envelope guests already
speak), with room to swap to a compact binary framing later behind the same ABI.

Rationale in one line: **option 2-over-3 is the only model that ships on the
runtime riz already has, while keeping every resiliency control in one host-side
seam.**

## Config shape

Capabilities are granted explicitly, per function, deny-by-default. The backends
(pools, buckets, credentials) are declared once at the top level and referenced
by name; the grant says *which* named resource a function may touch and *under
what limits*. Secrets never appear in the function block.

```toml
# ── Named backends (declared once; credentials live here, host-side) ──
[resources.pg.main]
# Works for Neon, Supabase, RDS, any Postgres — only the DSN changes.
dsn_env = "RIZ_PG_MAIN_DSN"   # e.g. postgres://…neon.tech/…  or  …supabase.co:6543/…
max_connections = 10
statement_timeout_ms = 2000

[resources.s3.assets]
bucket = "my-app-assets"
region = "us-east-1"
# credentials come from the host's standard AWS chain (env / role / profile)

# ── A function and the capabilities it is granted ──
[function.orders]
runtime = "wasm"
handler = "./examples/lambdas/orders-wasm/target/wasm32-wasip1/release/orders-wasm.wasm"
timeout_ms = 5000
concurrency = 4

[function.orders.capabilities.db]
type = "pg"
resource = "pg.main"          # references [resources.pg.main]
mode = "read-write"           # or "read-only"
max_inflight = 4              # concurrency cap for this function's PG calls
rate_per_sec = 50            # token-bucket rate limit
call_timeout_ms = 1500        # per-call deadline (capped by timeout_ms)
max_request_bytes = 65536
max_response_bytes = 1048576

[function.orders.capabilities.receipts]
type = "s3"
resource = "s3.assets"
prefix = "receipts/"          # the guest may only touch keys under this prefix
mode = "write-only"
max_inflight = 2
max_object_bytes = 5242880

# An http_fetch grant is an explicit origin allow-list — nothing else is reachable.
[function.orders.capabilities.tax_api]
type = "http"
allow_origins = ["https://tax.example.com"]
methods = ["POST"]
call_timeout_ms = 1000
rate_per_sec = 20
max_response_bytes = 262144
```

A function with **no** `[function.x.capabilities]` block has zero brokered
access — identical to today's behavior. Granting `db` lets the guest call
`pg_query("db", …)` and nothing more; the string `"db"` is the grant name, never
a DSN.

## Phasing

> **Phasing superseded (2026-07-19):** the threat model, dispatcher ordering and
> closed error set above carry forward unchanged, but the v1.1 KV / v2 S3+Dynamo /
> v2 http_fetch / v3 WIT phasing below is replaced by the capability suite + PR
> sequence in `2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html`.
> KV and S3 verbs are no longer planned for this cycle.

Marked clearly as **roadmap**. Nothing below is shipped.

- **v1 — Postgres-wire broker.** Implement the dispatcher + one verb,
  `pg_query`, against a host PG pool, with the full resiliency envelope
  (allow-list grant, per-call timeout, concurrency cap, rate limit, payload
  caps, audit). Because PG-wire is universal, v1 *is* Neon and Supabase support:
  point `dsn_env` at the provider's connection string. Custom-host-function
  interface (option 2) on `wasm32-wasip1`.
- **v1.1 — KV.** `kv_get` / `kv_put` over a PG-backed single table (reuses the
  v1 pool) so KV ships without a new backend dependency.
- **v2 — AWS storage.** `s3_get` / `s3_put` and DynamoDB-backed `kv_*` via the
  host AWS SDK + IAM. Prefix-scoped grants.
- **v2 — allow-listed `http_fetch`.** Origin allow-list egress for the long tail
  (webhooks, third-party APIs) the typed verbs don't cover.
- **v3 — Preview2 / component-model interface.** Republish the now-stable verb
  set as a WIT world; the v1 dispatcher becomes its host implementation. Guest
  authors opting into preview2 get typed bindings; preview1 imports remain
  supported.

Sequencing rationale: Postgres first because one verb unlocks the most backends
(Neon + Supabase + every PG) for the least code, and it exercises the entire
resiliency envelope end-to-end before we add backend variety.

## Non-goals (v1)

- No guest-initiated raw sockets, ever. If a verb doesn't cover it, it's not
  reachable.
- No long-lived guest-held connections or transactions spanning invocations —
  the host owns connection lifecycle; v1 brokered calls are self-contained.
- No credential material crossing the WASI boundary.
