# Shape-purity Tranche 6 (PR8 — dynamo capability) Implementation Plan

> Status: in-progress — PR8. Tranche of the 2026-07-19 spec
> `2026-07-19-lambda-shape-purity-and-wasm-capability-suite-design.html`.

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans.

**Goal:** A `dynamo` brokered capability — a WASM guest reads/writes DynamoDB through the broker: `dynamo.get_item | put_item | query | delete_item` over the DynamoDB JSON 1.0 HTTP API, signed host-side with SigV4. The guest sends item-level JSON, never a key or a signature; credentials live in the daemon env.

**Architecture:** `src/broker/dynamo.rs` — `DynamoBackend` holds region/table/endpoint + resolved credentials + the shared reqwest client. Each verb builds a DynamoDB JSON request (`X-Amz-Target: DynamoDB_20120810.<Op>`, body = the item/query JSON) and signs it with **aws-sigv4 + aws-credential-types** (maintained crates, not the full aws-sdk, not hand-rolled). Signing is strictly daemon-side. `mode = "read-only"` restricts the op set to `get_item`/`query`; grant `key_prefix` (optional) constrains partition-key values by prefix, enforced BEFORE signing. Plugs into the generalized dispatcher (`GrantBackend::Dynamo`, verbs matched in `run_verb`). Guest API: `riz_wasm::cap::dynamo::{get_item,put_item,query,delete_item}`.

## Global Constraints
- Same gates; `--squash --admin`. src/ under Power of 10.
- **Supply-chain**: add aws-sigv4 + aws-credential-types, run `cargo deny check` immediately; these are small maintained crates, NOT the full AWS SDK. If they trip the gate, stop and surface it (no hand-rolled SigV4).
- Signing correctness is THE test: a mock DynamoDB endpoint that independently re-signs the received request and BYTE-COMPARES the signature (proves canonicalization end-to-end), plus a structural unit test.

## Tasks
1. **Deps + gate**: `cargo add aws-sigv4 aws-credential-types` (+ whatever signing-output type is needed); `cargo deny check`. Commit.
2. **Config**: `[resources.dynamo.<name>]` → `DynamoResourceConfig { region, table, endpoint_url?, access_key_id_env?, secret_access_key_env?, session_token_env? }` (deny_unknown_fields); `CapabilityGrant.key_prefix: Option<String>`; `CAPABILITY_TYPES += "dynamo"`; validation cross-checks `[resources.dynamo.*]`. Commit.
3. **dynamo.rs backend**: `DynamoBackend::from_resource` (resolve creds from env or leave to a documented host chain; fail-fast if a named env is missing); `call(op, body, mode, key_prefix) -> Result<PgRows-single-row, String>` — op ∈ {GetItem,PutItem,Query,DeleteItem}; read-only rejects Put/Delete; key_prefix checked against the partition key in the request before signing; sign with aws-sigv4; POST to endpoint_url|`https://dynamodb.<region>.amazonaws.com`; return `{status, body}`. Unit test: signed request has a well-formed `AWS4-HMAC-SHA256` Authorization with the right credential scope + signed headers. Commit.
4. **Dispatcher wiring**: `GrantBackend::Dynamo`; `run_verb` arms `dynamo.get_item/put_item/query/delete_item`; `backends_for_function` builds the dynamo backend; `dispatch_inner` verb→type map gains the four dynamo verbs. Commit.
5. **riz-wasm cap::dynamo**: typed `get_item/put_item/query/delete_item` over `raw_call` with verbs; decode the `{status,body}` row. Commit.
6. **Pinned test** (`tests/broker_dynamo.rs`): a local mock DynamoDB endpoint that, per request, re-signs the received method/uri/headers/body with the same creds and **byte-compares the signature to the received one** (canonicalization proof), asserts `X-Amz-Target`, then answers a canned item. Through the Broker dispatcher: get_item returns the item; a read-only grant rejects put_item (backend error, never signed/sent); key_prefix rejects an off-prefix partition key. Commit.
7. **Docs + ship**: riz.toml.example dynamo block; CHANGELOG; PR "feat(broker): dynamo capability — SigV4-signed DynamoDB through the broker (PR8)"; deny.toml/SBOM note; merge --admin.

## Self-Review
Covers spec PR8: four verbs, resource+grant config, aws-sigv4 (not hand-rolled, gate-reviewed), daemon-side signing, read-only op restriction, key_prefix, mock-recompute-byte-compare test. Plugs into the PR7 generalized dispatcher. Names: DynamoBackend, DynamoResourceConfig, key_prefix, cap::dynamo.
