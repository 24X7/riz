# Release process

Riz uses [cargo-dist](https://opensource.axo.dev/cargo-dist/) for cross-platform release binaries. The whole release is a single `git tag` + `git push --tags`. No signing infrastructure, no manual artifact uploads.

## Cutting a release

```bash
# 1. Bump version in Cargo.toml + Cargo.lock
#    (cargo-dist refuses to release if the tag and Cargo.toml disagree)
sed -i '' 's/^version = ".*"/version = "0.1.0"/' Cargo.toml
cargo build  # regenerates Cargo.lock

# 2. Commit the version bump
git add Cargo.toml Cargo.lock
git commit -m "release: v0.1.0"

# 3. Tag and push
git tag v0.1.0
git push origin main
git push origin v0.1.0
```

Pushing the `v0.1.0` tag triggers `.github/workflows/release.yml`. cargo-dist will:

1. Run `cargo dist plan` to compute the artifact matrix
2. Build release binaries for each target in parallel:
   - `aarch64-apple-darwin` (Apple Silicon Macs)
   - `x86_64-apple-darwin` (Intel Macs)
   - `x86_64-unknown-linux-gnu`
   - `aarch64-unknown-linux-gnu`
3. Package each as `riz-<target>.tar.xz` with SHA256 sums
4. Generate the GitHub Release with release notes from commits
5. Upload the install shell installer (used by `https://riz.dev/install`)

Once the workflow completes (~5-10 min), the artifacts are live at:
`https://github.com/24X7/riz/releases/latest/download/riz-<target>.tar.xz`

The install script at `web/install` resolves these URLs and works automatically.

## What's NOT required

- **No Apple Developer ID / notarization.** macOS doesn't require code signing for CLI binaries installed via `curl | sh`. The quarantine attribute only fires when binaries are downloaded via a web browser. If a user does hit it (manual download case), they can clear it with: `xattr -d com.apple.quarantine /usr/local/bin/riz`. Major OSS CLIs (ripgrep, fd, bat, lazygit) all ship unsigned binaries this way.
- **No Windows signing.** Windows isn't a supported target.
- **No registry pushes.** crates.io publish (`cargo publish`) is optional and separate from the binary release.

## Smoke-testing the release after publish

Once the GitHub Actions workflow goes green:

```bash
# On a fresh machine (or via Docker):
curl -fsSL https://riz.dev/install | sh
$HOME/.local/bin/riz --version
$HOME/.local/bin/riz doctor
```

If `doctor` runs without crashing, the binary is good.

## Local pre-release dry-run

cargo-dist can simulate the build without tagging:

```bash
cargo install cargo-dist --version 0.22.1   # one-time
cargo dist plan        # show what would be built
cargo dist build       # actually build the artifacts locally
```

The artifacts land in `target/distrib/`. Untar one and run `./riz --version` to verify.

## Version bump conventions

`v0.<minor>.<patch>` for v0.x. Bump patch for bug fixes, minor for new features. The pre-1.0 contract is "no semver guarantees" — but in practice keep `riz.toml` parse-compatible across patches.

## If something goes wrong

The workflow runs on PR push too (without uploading). If the tag-push fails, check:

1. The `Cargo.toml` version matches the tag (cargo-dist enforces this)
2. The targets compile on all four platforms (some Rust crates use platform-conditional code — check the most recent green PR run)
3. The `[workspace.metadata.dist]` block in `Cargo.toml` is current — `cargo dist init` regenerates it if needed

To re-run a failed release: delete the tag, fix the issue, retag.

```bash
git tag -d v0.1.0
git push --delete origin v0.1.0
# ...fix...
git tag v0.1.0
git push origin v0.1.0
```

## TLS for `https://riz.dev/install`

The install URL itself is served via static hosting (riz.dev's web hosting setup — whichever you use). Make sure:

- The endpoint is HTTPS (Let's Encrypt or your CDN's TLS)
- `Content-Type: text/x-shellscript` or `text/plain` (some browsers will try to render shell scripts as HTML otherwise)
- No redirect chains that strip the trailing `/install` path

For Cloudflare Pages, Netlify, or any static host: drop the `web/install` file as `install` at the site root and add the right MIME type rule.
