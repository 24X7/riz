# riz integrations

Everything riz talks to is configured in `riz.toml` and validated at startup.
This directory documents the exact wiring + what each integration needs.

- **[observability.md](observability.md)** — OTLP trace export to Datadog,
  Honeycomb, Grafana Tempo, Jaeger, AWS X-Ray, or any OTel Collector; plus the
  Prometheus endpoint and the live-collector smoke test.
- **[otel-collector.yaml](otel-collector.yaml)** — a minimal Collector config
  used by the docs and `tests/telemetry_otlp_collector.rs`.

## LLM gateway providers (`[gateway]`)

Point any OpenAI client at `/_riz/v1/*` (`base_url` only). Providers:

| `kind` | endpoint (default `base_url`) | auth | needs |
|---|---|---|---|
| `openai` | `https://api.openai.com/v1` → `POST /chat/completions` | `Authorization: Bearer` | `api_key_env = "OPENAI_API_KEY"` |
| `anthropic` | `https://api.anthropic.com` → `POST /v1/messages` (`anthropic-version: 2023-06-01`) | `x-api-key` | `api_key_env = "ANTHROPIC_API_KEY"` |
| `ollama` | `http://localhost:11434/v1` → `POST /chat/completions` | none (local) | a running Ollama |
| `mock` | — (in-process) | none | nothing — deterministic, network-free (CI/offline default) |

```toml
[gateway]
default_provider = "anthropic"
fallback_chain   = ["anthropic", "openai"]
budget_usd       = 50.0                 # breach → HTTP 412

[gateway.providers.anthropic]
kind        = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"       # the env var name; the key never sits in config
[gateway.providers.openai]
kind        = "openai"
api_key_env = "OPENAI_API_KEY"
```

Routing: model-prefix (`anthropic/claude-...`) → `default_provider` → the
de-duplicated `fallback_chain`. Cost + tokens surface at `GET /_riz/v1/usage`.

## Auth — JWT / JWKS authorizers (`[function.x.authorizer]`)

riz validates RS256/ES256 JWTs against a provider's JWKS (works with Auth0,
Cognito, Okta, WorkOS, Clerk, or any standards-compliant IdP):

```toml
[function.api.authorizer]
type     = "jwt"
issuer   = "https://YOUR_TENANT.us.auth0.com/"
audience = "https://api.example.com"
jwks_uri = "https://YOUR_TENANT.us.auth0.com/.well-known/jwks.json"
```

JWKS keys are fetched and cached; verdicts are cached by source-IP + token hash.
The REQUEST authorizer type (`type = "request"`) instead calls one of your own
functions. The signature path is covered by `tests/auth_workos.rs` and
`tests/auth_clerk.rs`.

Admin endpoints under `/_riz/*` are bearer-gated when
`RIZ_AUTH_BEARER_TOKEN` (or `[auth] bearer_token`) is set (constant-time compare).
