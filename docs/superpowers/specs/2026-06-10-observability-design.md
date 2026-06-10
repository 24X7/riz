# Observability design — isolated telemetry process (Phase 2)

Status: 2a implemented; 2b–2d planned.
Branch: `claims-truth-ai-substrate`.

## Goal

Give `riz` first-class observability (request → function → chat-completion
span trees, GenAI token usage) **without ever letting telemetry add latency to,
or take down, the request path**. The whole design is organised around one
hard contract: *telemetry is best-effort and structurally isolated; the host
serves requests whether telemetry is healthy, slow, or dead.*

## Locked architecture

### Isolated telemetry process

Telemetry runs as a **dedicated child process of the same binary**:

```
riz __telemetry <sink-path>
```

This mirrors the existing `__wasm-host` precedent (`src/main.rs`): the child is
early-dispatched in `main()` **before** the tokio runtime is constructed, so the
worker stays lean (no multi-threaded scheduler per child) and is fully isolated
from the host event loop and the function pools.

Exe resolution for spawning the child **mirrors `src/process/wasm.rs`**: it
honours the `RIZ_HOST_BIN` override env var first, then `current_exe()`, then a
`"riz"` PATH fallback. This is load-bearing under `cargo nextest`, where
`current_exe()` is the test runner rather than the real `riz` binary — the same
override the WASM host relies on lets integration tests launch the real binary.

Why a separate **process** (not just a task/thread): a panic, an OOM, a hang, or
a slow exporter inside telemetry is contained behind an OS process boundary. The
host's only coupling to it is a bounded channel and a pipe it can drop.

### Host-resiliency contract (the point)

The host emits telemetry through a **bounded, non-blocking** channel:

- `TelemetryHandle::emit(&self, ev)` does a non-blocking `try_send` on a bounded
  `tokio::sync::mpsc` (capacity from config, default 4096). It **never awaits,
  never blocks, never fails the request path.**
- On `Err(Full)` (queue saturated — child slow/stalled) or `Err(Closed)` (drain
  task gone / child dead), the event is **dropped** and an `AtomicU64` dropped
  counter is incremented. `emit` returns immediately, O(1).
- `TelemetryHandle::dropped() -> u64` exposes the counter (surfaced later in the
  dev TUI and as an internal metric).
- `TelemetryHandle::disabled()` is a no-op handle (no channel) whose `emit`
  always drops — used when `[telemetry].enabled = false` so call sites are
  unconditional.

A **drain task** owns the receiver end: it pulls events and writes them
length-prefixed to the child's stdin. A **supervisor** spawns the child, owns
its stdin, watches for child exit, and **respawns** with bounded exponential
backoff. If telemetry crashes mid-flight, in-flight `emit`s keep dropping (never
blocking) until a fresh child is up, at which point new events flow to the new
sink.

Net guarantee: *telemetry being slow or crashed can add neither latency nor
failure to serving requests.* Proven by the three isolation tests.

## Wire format (IPC)

`TelemetryEvent` (serde) is, for 2a, a general span event:

```rust
struct TelemetryEvent {
    name: String,
    kind: SpanKind,            // Server | Internal | Client
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    start_unix_nanos: u64,
    end_unix_nanos: u64,
    attributes: BTreeMap<String, AttrValue>, // String | Int | Double | Bool
}
```

Framing: `write_frame` = `u32`-LE length prefix + `serde_json` bytes;
`read_frame` reads the prefix then exactly that many bytes, returning `None` on
clean EOF and being robust to partial reads. Length-prefixing (not newline
delimiting) keeps the format binary-safe and trivially resyncable.

## Phase split

- **2a (this phase):** isolated `__telemetry` process, bounded non-blocking
  emitter, drain task, supervisor + respawn, IPC wire format. The worker just
  **deserializes events and appends them as JSON lines to the sink file** given
  on argv. This proves IPC + isolation end-to-end and leaves a clean seam where
  the real exporter slots in. **No OTLP export yet.**
- **2b:** real **OTLP/HTTP export with JSON encoding, hand-rolled** — no
  `opentelemetry` / `opentelemetry-sdk` / `opentelemetry-otlp` / `tonic` /
  `prost` crates. Rationale: the product is "one ~10MB Rust binary"; we will not
  bloat it with the OTel crate tree. `reqwest 0.12` (json + blocking) and
  `serde_json` are already deps and are sufficient to POST OTLP/HTTP-JSON to a
  collector `endpoint` with configured `headers`. The 2a sink-file worker is the
  exact seam this replaces. Also removes the legacy `[datadog]` config field.
- **2c:** dev-TUI telemetry panel (`riz run --dev` only) — live span tree, drop
  counter, child health/respawn count.
- **2d:** span instrumentation wired into the request → function →
  chat-completion path, populating OTel **GenAI semantic-convention** token
  attributes:
  - `gen_ai.usage.input_tokens`
  - `gen_ai.usage.output_tokens`
  - `gen_ai.request.model`
  - `gen_ai.system`

  Planned span tree: a **request** server span (HTTP route) → **function**
  internal span (pool invocation) → **chat-completion** client span (LLM call,
  carrying the GenAI attrs above).

## Config (`[telemetry]`)

```toml
[telemetry]
enabled = false                    # default; disabled => no child, no channel
endpoint = "http://localhost:4318" # OTLP/HTTP collector (used in 2b)
queue_capacity = 4096              # bounded emit channel
[telemetry.headers]                # OTLP export headers (2b), e.g. auth
# "x-api-key" = "..."
```

`TelemetryConfig` derives `Default` and every field is `#[serde(default)]`, so
adding it to `Config` is non-breaking: existing configs and tests that build a
`Config` keep compiling. The existing `[datadog]` field is left untouched here;
its removal is 2b.

## Module layout

```
src/observability/
  mod.rs       TelemetryHandle (clone-able emitter), TelemetrySupervisor,
               TelemetryHandle::disabled()
  ipc.rs       TelemetryEvent, SpanKind, AttrValue, write_frame/read_frame
  process.rs   run_worker(): the `riz __telemetry <sink>` entry (blocking)
```

`main.rs` early-dispatches `__telemetry` next to `__wasm-host`; `lib.rs` exposes
`pub mod observability`.

## Tests (proofs)

`tests/telemetry_process_isolation.rs`:

1. **emit_never_blocks_and_drops_on_overflow** — tiny-capacity channel with no
   drain running; fire far more events than capacity; assert every `emit`
   returns within a tight time bound and `dropped() > 0`. Host never blocks.
2. **telemetry_child_roundtrips_events_to_sink** — real supervisor → child →
   temp sink; emit events; bounded-poll the sink until the deserialized JSON
   lines match. IPC + isolated process work end-to-end under nextest (validates
   the `RIZ_HOST_BIN` exe-resolution override).
3. **host_emit_survives_child_kill_and_supervisor_respawns** — kill the child;
   assert `emit` still returns immediately; assert the supervisor respawns and a
   subsequently-emitted event reaches the new sink. Crash can't block/kill the
   host; recovery works.
