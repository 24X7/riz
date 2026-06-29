# Observability integrations — OTLP, Datadog, Honeycomb, Tempo, X-Ray

riz exports **traces** as **OTLP/HTTP-JSON** to `<endpoint>/v1/traces` with
`Content-Type: application/json`. Span/trace ids are emitted as spec-correct hex
(32-char trace, 16-char span), times as stringified unix-nanos, with a
`service.name = riz` resource attribute — so any OTLP-compatible backend accepts
them. There is exactly **one** export path: every backend below is just a
different `endpoint` + `headers`.

Enable it in `riz.toml`:

```toml
[telemetry]
enabled  = true
endpoint = "http://localhost:4318"   # an OTLP/HTTP collector or agent base URL

[telemetry.headers]                  # optional; added verbatim to each POST
# x-honeycomb-team = "..."
```

riz also exposes **Prometheus** metrics at `/_riz/metrics` (scrape it directly,
or via the Datadog Agent's OpenMetrics integration) — independent of the OTLP
trace path above.

> riz exports traces over OTLP. It does **not** speak any vendor's native wire
> protocol (no StatsD, no X-Ray UDP). Point it at a collector/agent and the
> collector fans out to your vendor.

## Datadog

The GA path is an **OTLP receiver** you run — either the **Datadog Agent** (with
OTLP ingest enabled) or an **OpenTelemetry Collector** with the `datadog`
exporter. riz posts to it; it forwards to Datadog.

```toml
[telemetry]
enabled  = true
endpoint = "http://localhost:4318"   # Datadog Agent / Collector OTLP/HTTP receiver
```

Datadog Agent: enable OTLP ingest (`otlp_config.receiver.protocols.http.endpoint:
0.0.0.0:4318`). The agent's OTLP receiver accepts OTLP/HTTP JSON.

riz's LLM-gateway spans carry the current OTel **GenAI** attributes
(`gen_ai.operation.name`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`,
`gen_ai.usage.output_tokens`, `gen_ai.provider.name`), so Datadog **LLM
Observability** classifies them automatically.

> **Agentless caveat:** Datadog's direct (agentless) OTLP intake with a
> `dd-api-key` header is GA for **metrics/logs** but **Preview for traces**
> (request access from your Datadog CSM). Until then, use the Agent/Collector
> path above — it works today. If you have Preview access:
> ```toml
> [telemetry]
> endpoint = "https://trace.agent.datadoghq.com"   # per Datadog's OTLP intake docs
> [telemetry.headers]
> dd-api-key = "${DD_API_KEY}"
> ```

## Honeycomb

```toml
[telemetry]
enabled  = true
endpoint = "https://api.honeycomb.io"
[telemetry.headers]
x-honeycomb-team    = "${HONEYCOMB_API_KEY}"
# x-honeycomb-dataset = "riz"   # classic keys only
```

## Grafana Tempo / Jaeger / generic OTel Collector

Any OTLP/HTTP receiver works with just an endpoint (Jaeger all-in-one and Tempo
both expose `:4318`):

```toml
[telemetry]
enabled  = true
endpoint = "http://localhost:4318"
```

## AWS CloudWatch / X-Ray

Run an **ADOT** (AWS Distro for OpenTelemetry) or OTel Collector with the X-Ray /
CloudWatch exporter and point riz at its OTLP receiver (`:4318`). riz → collector
→ X-Ray.

## Verify it end-to-end

A real OpenTelemetry Collector confirms the wire is accepted:

```bash
docker run --rm -p 4318:4318 \
  -v "$PWD/docs/integrations/otel-collector.yaml":/etc/otelcol/config.yaml \
  otel/opentelemetry-collector:latest --config /etc/otelcol/config.yaml
```

Then run riz with `endpoint = "http://localhost:4318"`, hit a route, and watch
riz's spans (`service.name=riz`, hex ids) appear in the collector's output. This
is automated by `tests/telemetry_otlp_collector.rs`:

```bash
RIZ_OTLP_DOCKER=1 cargo nextest run --test telemetry_otlp_collector
```
