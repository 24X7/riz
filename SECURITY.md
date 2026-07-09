# Security Policy

> Posture overview — what the sandbox, process profile, Landlock, and rlimits
> each protect, and exactly where confinement stops:
> <https://riz.dev/security.html>.

## Supported versions

riz is pre-1.0. Security fixes land on the latest `0.1.x` release and `main`.

| Version | Supported |
|---------|-----------|
| latest `0.1.x` | ✅ |
| older          | ❌ (please upgrade) |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately via GitHub's **Security → "Report a vulnerability"**
(private advisory) on this repository, or email **chris@riz.dev**.

Please include:
- a description and impact,
- a minimal `riz.toml` + handler (or steps) to reproduce,
- the riz version (`riz --version`), OS/arch, and runtime (bun/node/python/rust/wasm).

We aim to acknowledge within **3 business days** and to ship a fix or mitigation
for confirmed, in-scope issues as quickly as is practical, coordinating
disclosure with you.

## Scope

Especially interested in: sandbox/capability escapes (`runtime = "wasm"`, the
resource broker, `guard_in`/`guard_out`), auth bypass (JWT/JWKS authorizers,
bearer-gated `/_riz/*`), path traversal in static serving, the process safety
profile, and the `POST /deploy` path.

Out of scope: issues that require a misconfiguration riz already rejects at
startup, or non-HTTP/WS AWS event sources (out of project scope by design).
