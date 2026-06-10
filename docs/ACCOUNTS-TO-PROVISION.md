# Accounts to Provision

Living ledger of every external service the team must sign up for to **test and operate** the
[Claims-Truth & AI-Substrate roadmap](superpowers/plans/2026-06-09-claims-truth-and-ai-substrate.md).
Check off each row as it is provisioned; fill in the **Notes** column with the real value.

**Last updated: 2026-06-09**

---

## 1. Auth (Phase 3)

| Service | Why we need it / phase | What's needed | Status | Notes |
|---|---|---|---|---|
| **Clerk** | JWT/JWKS auth integration tests — verify riz validates Clerk-issued RS256/ES256 tokens against a real JWKS endpoint (Phase 3) | Test tenant created, JWKS URL (`https://<tenant>.clerk.accounts.dev/.well-known/jwks.json`), at least one test user token | `[ ] not started` | |
| **WorkOS** | JWT/JWKS auth integration tests — verify riz validates WorkOS-issued tokens (Phase 3) | Test organization/tenant created, JWKS URL (`https://api.workos.com/sso/jwks/<client_id>`), test IdP connection, sample token | `[ ] not started` | |

---

## 2. Observability (Phase 2)

| Service | Why we need it / phase | What's needed | Status | Notes |
|---|---|---|---|---|
| **Datadog** | OTLP intake target for the riz telemetry pipeline; proves the Datadog exporter end-to-end (Phase 2) | Free/trial account, **OTLP intake endpoint** (e.g. `https://api.datadoghq.com`), **API key** set in `DD_API_KEY` env var | `[ ] not started` | |
| **AWS CloudWatch / X-Ray** | Second OTLP sink; proves the CloudWatch + X-Ray exporter path and the OTLP→X-Ray segment mapping (Phase 2) | AWS account (can be existing), **IAM role or user** with `xray:PutTelemetryRecords`, `logs:PutLogEvents`, `logs:CreateLogGroup`, `logs:CreateLogDelivery`; **X-Ray / CloudWatch OTLP endpoint** (`https://xray.<region>.amazonaws.com`) | `[ ] not started` | |

---

## 3. AI / Gateway (Phase 4)

| Service | Why we need it / phase | What's needed | Status | Notes |
|---|---|---|---|---|
| **Anthropic** | Claude Agent SDK examples — the flagship agent-loop demo uses model `claude-opus-4-8` via `/_riz/v1/*`; also used for gateway compat tests against the real Anthropic Messages API (Phase 4) | **API key** set in `ANTHROPIC_API_KEY` env var; sufficient quota for `claude-opus-4-8` requests in CI smoke runs | `[ ] not started` | |
| **OpenAI** | Optional gateway compat tests — verify the riz LLM gateway correctly proxies the OpenAI-shaped wire protocol against the real OpenAI API (Phase 4, optional) | **API key** set in `OPENAI_API_KEY` env var; gated behind env-var presence in CI so tests skip cleanly when key absent | `[ ] not started` | |

---

## 4. WASM Brokered Resources (Phase 5, roadmap)

| Service | Why we need it / phase | What's needed | Status | Notes |
|---|---|---|---|---|
| **Neon** | Postgres-wire brokered resource target — WASM guests will request Postgres queries that the riz host executes under policy; Neon is the serverless PG target (Phase 5 roadmap) | Free project/database, **connection string** (`postgresql://user:pass@<host>/db`), branch name for test isolation | `[ ] not started` | |
| **Supabase** | Second Postgres-wire target (same PG-wire protocol as Neon, different hosted provider) — broadens brokered-resource coverage (Phase 5 roadmap) | Free project/database, **connection string** (`postgresql://postgres:<pass>@db.<ref>.supabase.co:5432/postgres`), test schema | `[ ] not started` | |
| **AWS S3** | Brokered S3 object access — WASM guests will request S3 gets/puts that the host executes under allow-list policy (Phase 5 roadmap) | Dedicated test bucket (e.g. `riz-wasm-test`), **IAM role/user** with `s3:GetObject`, `s3:PutObject` scoped to that bucket, bucket region | `[ ] not started` | |
| **AWS DynamoDB** | Brokered DynamoDB access — WASM guests request table reads/writes via the host broker (Phase 5 roadmap) | Test table (e.g. `riz-wasm-test`), **IAM role/user** with `dynamodb:GetItem`, `dynamodb:PutItem`, `dynamodb:Query` scoped to that table | `[ ] not started` | |

---

## 5. Payments to Chris

| Channel | Why / what to set up | What's needed | Status | Notes |
|---|---|---|---|---|
| **GitHub Sponsors** | Primary recurring-fiat channel for riz supporters; handle already exists | Enable Sponsors on the `24X7` GitHub account (`https://github.com/sponsors/24X7`); set up payout bank/PayPal; add "Sponsor" button to the repo | `[ ] not started` | |
| **Buy Me a Coffee** | One-time / low-friction tip channel | Create account at `buymeacoffee.com/24X7`; link from `riz.dev` and `README.md` | `[ ] not started` | |
| **ETH wallet** | Crypto tip / donation channel — replaces `<placeholder>` on riz.dev | Ethereum wallet address (e.g. MetaMask / hardware wallet) | `[ ] not started` | `<ETH_WALLET_ADDRESS>` |
| **BTC wallet** | Bitcoin tip channel — replaces `<placeholder>` on riz.dev | Bitcoin wallet address | `[ ] not started` | `<BTC_WALLET_ADDRESS>` |
| **SOL wallet** | Solana tip channel — replaces `<placeholder>` on riz.dev | Solana wallet address | `[ ] not started` | `<SOL_WALLET_ADDRESS>` |
| **Stripe / Open Collective** | Consider if recurring fiat donations beyond GitHub Sponsors are wanted (e.g. for organizations that need invoice/receipt flows) | Stripe account or Open Collective collective; link from site | `[ ] not started` | Not urgent — evaluate after Sponsors is live |
