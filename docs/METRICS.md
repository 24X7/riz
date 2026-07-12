# riz Metrics — design for a demanding production runtime

`/_riz/metrics` emits Prometheus text format 0.0.4. This document is the
intent: what a Lambda-shaped web-API runtime must expose to be operable at
scale, why, and what is shipped vs. staged. It is bound by the goal — a service
a frontier lab could bet an SLA on, where taking one function to that
reliability is trivial — not by what happened to exist first.

## Principles

1. **Cover the four golden signals** — latency, traffic, errors, saturation —
   plus the runtime's own reliability (worker supervision) and efficiency
   (cache).
2. **Right metric type.** Counters for monotonic totals, gauges for levels,
   histograms for latency (see the summary caveat below).
3. **Bounded cardinality.** Labels are limited to values fixed by config:
   `function`, `route`, `quantile`, `version`. Never per-request-id, per-path,
   or per-user — unbounded label sets are the classic way to take down a
   Prometheus. A deployment's series count is O(functions × metrics).
4. **Just works, with an off switch.** Enabled by default; `[metrics] enabled
   = false` removes the endpoint. Bearer-auth protects it when a token is set.
   No per-metric config to get wrong.

## Shipped

**Traffic / errors (counters, per function)**
`riz_invocations_total`, `riz_errors_total`, `riz_cold_starts_total`.

**Saturation — the signal a warm-pool runtime lives or dies by (per function)**
- `riz_concurrency_limit` (gauge) — configured permits.
- `riz_concurrency_in_use` (gauge) — permits held now. `in_use / limit` is
  utilization; at 1.0 the next request is shed. This is what tells you to raise
  concurrency or add instances *before* you start dropping traffic.
- `riz_admission_rejected_total` (counter) — requests load-shed at the limit.
  A rising rate is the definitive "you are overloaded" signal.
- `riz_workers` (gauge) — live worker processes in the pool.
- `riz_pool_memory_bytes` (gauge) — resident memory across the pool.

**Worker reliability (supervision)**
- `riz_worker_restarts_total` (counter) — respawns (crash or timeout).
- `riz_worker_consecutive_crashes` (gauge) — proximity to the crash-loop
  circuit breaker.
- `riz_function_healthy` (gauge) — 1 if the pool is serving.

**Latency (both forms)**
- `riz_request_duration_seconds` (**histogram**, per function) — `_bucket{le=…}`
  + `_sum` + `_count`, cumulative monotonic counters. This is the aggregatable
  form: a scraper computes a fleet-wide quantile from the buckets, which a
  summary cannot.
- `riz_latency_ms` (summary, per function) — pre-computed p50–p99 over a 5-min
  window, the same numbers the TUI shows live. Kept for one release; prefer the
  histogram for anything aggregated. Deprecation noted in its `# HELP`.

**Efficiency / meta**
`riz_cache_hits_total`, `riz_cache_misses_total` (counters),
`riz_uptime_seconds`, `riz_build_info{version}` (gauges).

## Staged (designed, not yet emitted) — with rationale

1. **HTTP status-class breakdown.** `riz_responses_total{function,
   status_class="2xx|4xx|5xx"}`. `errors_total` doesn't separate client faults
   from server faults; RED dashboards need the split. Staged because it
   requires recording the class on the response path.
3. **Cold-start duration**, not just count — how long a spawn takes when the
   warm pool misses.
4. **OTLP metrics push**, alongside scrape, for pull-averse environments (traces
   already export over OTLP).

## Non-goals

Per-request or per-path series (cardinality blowup); business metrics (those
belong in the functions); auth-token or PII labels.
