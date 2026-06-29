<!-- Thanks for contributing to riz! -->

## What & why

<!-- What does this change and why? Link any issue: "Closes #123". -->

## How it was tested

<!-- Commands you ran. riz uses cargo-nextest, never `cargo test`. -->

```
cargo nextest run
```

## Checklist

- [ ] `cargo nextest run` passes
- [ ] `cargo clippy --all-targets` is clean for changed code
- [ ] Docs updated if behavior/config changed (README, `docs/`, or `web/`)
- [ ] If this adds a capability claim to the website, it's backed by a passing
      test in `tests/claims/registry.toml` (claims-as-code)
- [ ] Scope respected: HTTP/WebSocket Lambdas + the agent/AI surface only
