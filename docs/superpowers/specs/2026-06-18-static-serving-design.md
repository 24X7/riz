# Static-file serving — colocate your site with your API (design)

Status: **SHIPPED** (v1 + v1.1 + v2 all implemented and tested — see Phasing).
Branch: `claims-truth-ai-substrate`.
Last updated: 2026-06-29.

## Goal

Let a riz instance serve static files (an SPA build, a landing page, docs, the
agent-discovery files) from the same `~35 MB` binary and the same port as the
API — **no second host, no CORS, no extra infra**. One sentence: *colocate your
frontend with your functions; a running riz instance can also describe itself
to agents.*

## Positioning guardrail (read first)

This is a **scoped quality-of-life feature, not a pillar.** It does NOT make riz
"a web server" — the compare page's line stands ("HTML templating, sessions,
ORMs are not riz's job"). Two framings are allowed, and only these:

1. **Colocation** — "serve your SPA/landing on the same binary and origin as
   your API." Removes the "...but where do I host the frontend?" friction that
   works against the one-binary pitch.
2. **Self-describing instance** — a live riz can serve its own `llms.txt`,
   `.well-known/riz.json`, and a tiny landing that points agents at
   `/_riz/mcp`. This is the framing that *compounds* with "every function is an
   agent's tool"; lead with it.

Marketing: one paragraph on the docs page + one fact on the compare table
("your site, same binary — `$0`, no CORS"). **No new homepage pillar, no
cap card.** If the implementation tempts you toward templating, rewrites, proxying,
or a plugin system — stop; that is out of scope (see Non-goals).

## Precedence (exact — this is the whole correctness story at the routing layer)

riz already matches in this order in `build_app` + `dispatch_lambda`
(`src/server.rs`): fixed system routes (`/health`, `/ready`, `/deploy`,
`/cache/invalidate`) → `/_riz/mcp` → WebSocket upgrade routes → `/_riz/v1/*`
gateway → `.fallback(dispatch_lambda)` which matches `[function.*]` routes via
the Router, else 404.

Static slots in as the **last fallback before that 404**, gated so it can never
shadow an API:

```
request
  → system / gateway / mcp / ws routes          (unchanged, always win)
  → function route?  router.function_for_path(path).is_some()   → dispatch to function (incl. its own 405/404)
  → NOT a function path, method is GET or HEAD, [static] configured, path under mount?
        → serve static file  (or SPA fallback)   ← NEW
  → otherwise                                     → 404 (unchanged)
```

Rules, non-negotiable:
- **Functions and `/_riz/*` always win.** The gate is
  `function_for_path(path).is_none()` — the *same* method-agnostic lookup that
  already drives CORS preflight in `dispatch_lambda`. If any function owns the
  path, static is never consulted (so a function can't be silently shadowed,
  and its method-mismatch still yields the function's 405/404).
- **GET/HEAD only.** Static is never a POST/PUT/DELETE target.
- **`/_riz/` and the configured system prefixes are reserved** — never served
  from disk even if a file with that name exists under the static dir.
- Disabled by default: no `[static]` block ⇒ behavior is exactly today's.

## Config shape

```toml
[static]
dir = "./public"          # directory served as the site root (required to enable)
# Optional, with the shown defaults:
mount = "/"               # URL prefix the dir is served under
index = "index.html"      # directory-index file (served for a dir request)
spa_fallback = false      # unknown non-API GET → serve `index` (history-API SPAs)
not_found = ""            # optional custom 404 file (e.g. "404.html"); else plain 404
precompressed = false     # serve file.gz / file.br when present + Accept-Encoding allows
# Cache policy — sane defaults below; immutable assets are detected by a content hash
# in the filename (e.g. app.4f1c2a.js), which get a 1-year immutable cache.
cache_html = "no-cache"   # index/html: revalidate every time (so deploys are seen)
cache_assets = "public, max-age=3600"   # non-hashed assets
cache_immutable = "public, max-age=31536000, immutable"  # hash-named assets
```

`validate()` checks (fail-closed at startup, the riz way):
- `dir` exists and is a directory (else clear startup error — never a silent
  empty mount).
- `mount` starts with `/` and is NOT `/_riz` or a prefix that collides with a
  declared function route or a system endpoint (reject with a message naming
  the collision).
- `index` / `not_found` resolve to files inside `dir` (no `..`).

## Correctness checklist (the boring parts, done right)

riz's brand is "we get the boring parts right," so a half-`ServeDir` would
undercut it. Recommended: build on **`tower-http`'s `ServeDir`/`ServeFile`**
(feature `fs`) — it already handles the long tail below correctly — invoked as a
`oneshot` tower service from the static fallback (NOT mounted as a competing
axum fallback, so the precedence above is preserved). Each item below must be
covered, by ServeDir or explicitly:

- **Content-Type** from extension (`mime_guess`); UTF-8 charset for text types;
  correct types for `.wasm`, `.json`, `.svg`, `.txt`, `.webmanifest`.
- **Conditional requests** — `ETag` (or strong `Last-Modified`) + honor
  `If-None-Match` / `If-Modified-Since` → `304`.
- **Range requests** — `Accept-Ranges: bytes`, honor `Range` → `206` (and
  `416` on an unsatisfiable range). Matters for video/large assets.
- **Precompressed** — when `precompressed = true`, serve `path.br` / `path.gz`
  for a client that sent `Accept-Encoding`, with `Content-Encoding` + `Vary:
  Accept-Encoding`. (No on-the-fly compression in v1.)
- **Caching headers** — per the `cache_*` policy; immutable for hash-named files,
  `no-cache` for HTML so a redeploy is picked up immediately.
- **Directory request** → serve `index`; a request for `dir/` with no index →
  404 (no autoindex; never list a directory).
- **HEAD** returns headers with no body.
- **SPA fallback** (when enabled) — an unknown GET whose `Accept` includes
  `text/html` and that is NOT an API path and NOT a request for a file with an
  extension (so a missing `/foo.js` still 404s, not index.html) → serve `index`
  with `200` (history-API routing). A missing asset must 404, not return HTML.

## Security (the part that bites)

- **Path traversal** — canonicalize the resolved path and assert it stays
  inside `dir`; reject `..`, encoded `%2e%2e`, absolute paths, and NUL bytes.
  ServeDir does this; the test below pins it regardless.
- **Symlink escape** — do not follow a symlink that resolves outside `dir`.
- **Dotfiles** — do not serve `.`-prefixed files by default EXCEPT the
  explicitly-allowed agent surface (`/.well-known/*`). `.git`, `.env`, etc.
  must 404.
- **Reserved prefixes** — `/_riz/*` is never served from disk (guard even if a
  `public/_riz/...` file exists).
- The static path inherits the **same bearer/CORS posture as the rest of the
  server** only where it already applies; static assets are public by default
  (they're a website), but the precedence guarantees an auth-gated `/_riz/*` is
  never reachable via static.

## Agent-discovery angle (the compounding bit — build this, not just file-serving)

If `[static]` is set and the dir contains `llms.txt` and/or
`.well-known/riz.json`, riz serves them at `/llms.txt` and
`/.well-known/riz.json` — so **a live instance is self-describing**: an agent
pointed at `https://my-host/` can fetch the when-to-use card and discover the
MCP endpoint without a separate marketing site. Optional `riz init` could drop a
starter `public/llms.txt` + `.well-known/riz.json` templated from the running
config (function list → tool list). Document this as the headline use, with
colocation second.

## Implementation sketch

- `src/config.rs` — `StaticConfig` (+ `Config.r#static: Option<StaticConfig>`),
  with `serde(default)`; `validate()` additions above. (Sweep the
  `Config { … }` literals like the `mcp` / `capabilities` fields were.)
- `src/static_files.rs` (new) — `async fn serve(parts: &http::request::Parts,
  cfg: &StaticConfig) -> Option<Response>`: returns `Some(resp)` when it
  resolves a file / index / spa-fallback / custom-404, `None` to let the caller
  fall through to the normal 404. Internally wraps `tower-http` ServeDir or a
  hand-rolled safe resolver covering the checklist.
- `src/server.rs::dispatch_lambda` — before building the AWS event, after the
  existing CORS branch: `if req.method() is GET/HEAD && function_for_path(path).is_none()
  && state.config.read().await.r#static.is_some()` → call
  `static_files::serve(...)`; if `Some`, return it; else continue to the normal
  path (which will 404). Keep the `Request` parts available (don't consume the
  body until after this branch).
- `Cargo.toml` — `tower-http = { version = "0.6", features = ["fs"] }` (or hand
  roll to preserve the lean binary; decide at build time, note in the PR).
- `[static]` does NOT spawn a pool, touch the broker, or interact with WASM —
  it is pure HTTP-layer file serving.

## Tests (hold the line) + claims-registry entries

`tests/static_serving.rs` (real `build_app` in-process, a `tempdir` web root):
- `serves_index_for_root_and_directory`
- `function_route_wins_over_static_file_at_same_path`  ← precedence keystone
- `riz_system_path_is_never_served_from_disk`          ← `public/_riz/x` ignored
- `path_traversal_dotdot_is_rejected`                  ← security keystone
- `dotfiles_are_hidden_except_well_known`
- `spa_fallback_serves_index_for_unknown_html_route_but_404s_missing_asset`
- `conditional_request_returns_304` / `range_request_returns_206`
- `content_type_is_correct_for_wasm_json_svg`
- `head_returns_headers_no_body`
- `immutable_cache_header_on_hash_named_asset_no_cache_on_html`
- `live_instance_serves_its_llms_txt_and_well_known`   ← the agent angle
- config: `static_dir_missing_is_a_startup_error`,
  `static_mount_colliding_with_function_route_is_rejected`

Claims registry (`tests/claims/registry.toml`) — only if/when it appears on the
site, and only proven:
- `static-colocation` → proof `function_route_wins_over_static_file_at_same_path`
- `static-self-describing` → proof `live_instance_serves_its_llms_txt_and_well_known`
- `static-traversal-safe` → proof `path_traversal_dotdot_is_rejected`

## Non-goals (v1 — say no, loudly)

- No templating / SSR / view engine.
- No reverse-proxy / upstream pass-through.
- No on-the-fly compression (precompressed files only).
- No autoindex / directory listing.
- No per-file auth rules or a config DSL — static assets are public; gated
  surfaces stay behind `/_riz/*` and functions.
- No file watching / live-reload of the static dir (it's a deploy artifact;
  `POST /deploy` / restart is the update path).
- No CDN behaviors riz can't beat (edge cache, HTTP/3) — for scale, front riz
  with a CDN; static here is for the small/colocated/self-hosted case.

## Phasing

- **v1** — ✅ SHIPPED. `[static]` mount + index + traversal-safe serving +
  content-type + conditional/range + cache headers + the precedence gate + the
  agent files (`src/static_files.rs`, `src/config.rs`, `dispatch_lambda`;
  `tests/static_serving.rs`). This is the whole valuable core.
- **v1.1** — ✅ SHIPPED alongside v1. `spa_fallback`, custom `not_found`,
  `precompressed` are all implemented and tested.
- **v2** — ✅ SHIPPED. `riz scaffold static [dir] [--mount] [--wire] [--force]`
  generates `<dir>/llms.txt` + `<dir>/.well-known/riz.json` DERIVED from the
  project's `riz.toml` — every `[function.*]` becomes a tool entry (name,
  runtime, routes, description) matching what `/_riz/mcp` advertises; `--wire`
  appends the `[static]` block. Implemented as a pure, testable generator in
  `src/scaffold.rs` (not a static template — it reads the actual config), wired
  in `src/main.rs`; covered by `tests/static_scaffold.rs` including the
  round-trip that proves a live `[static]` instance serves its own generated
  discovery files.

Sequencing rationale: the precedence gate + traversal safety + the agent files
are the parts that must be right and that carry the on-brand value; SPA/compression
are conveniences layered after; v2 scaffolding makes the self-describing-instance
framing turnkey.

Also update the status line at the top of this doc.
