//! `riz init` template fetching — ALWAYS from a git location, never embedded.
//!
//! Templates are not baked into the binary. `riz init <spec>` resolves `<spec>`
//! to a source and copies it into the target directory:
//!
//!   * a built-in name (`typescript-http`, `typescript-todo`, …) → a subdir of
//!     the official riz repo, fetched by a shallow `git clone`;
//!   * `owner/repo`, `owner/repo/sub/dir`, optionally `#ref` → any GitHub repo
//!     or subdirectory, so anyone can publish and use their own templates;
//!   * a full git URL (`https://…`, `git@…`, `ssh://…`, `file://…`), optionally
//!     `#ref`; or a GitHub "tree" browser URL (ref + subdir are extracted);
//!   * a local filesystem path (existing dir / `./…` / `/…` / `~…`) — handy
//!     offline and for testing.
//!
//! The default repo is overridable with `RIZ_TEMPLATE_REPO` (a git URL or a
//! local path), which is how the test-suite stays hermetic.

use std::path::{Path, PathBuf};

/// The official template repo. Built-in names resolve to a subdir of this.
/// Overridable via `RIZ_TEMPLATE_REPO` (git URL or local path) for forks/tests.
const DEFAULT_REPO: &str = "https://github.com/24X7/riz";

/// Built-in templates: (name, subdir-in-repo, scenario, language). These are
/// the "out of the box" set `riz init --list` shows — still fetched from git,
/// never embedded. Anyone can also point `riz init` at their own `owner/repo`.
pub const BUILTINS: &[(&str, &str, &str, &str)] = &[
    (
        "typescript-http",
        "templates/typescript-http",
        "HTTP",
        "TypeScript / Bun",
    ),
    ("python-http", "templates/python-http", "HTTP", "Python"),
    ("rust-http", "templates/rust-http", "HTTP", "Rust"),
    ("nodejs-http", "templates/nodejs-http", "HTTP", "Node.js"),
    ("go-http", "templates/go-http", "HTTP", "Go"),
    (
        "wasm-http",
        "templates/wasm-http",
        "HTTP",
        "Rust → wasm32-wasip1 (WASI sandbox)",
    ),
    (
        "typescript-websocket",
        "templates/typescript-websocket",
        "WebSocket",
        "TypeScript / Bun",
    ),
    (
        "python-websocket",
        "templates/python-websocket",
        "WebSocket",
        "Python",
    ),
    (
        "rust-websocket",
        "templates/rust-websocket",
        "WebSocket",
        "Rust",
    ),
    (
        "typescript-todo",
        "examples/typescript-todo",
        "Full-stack",
        "TS/Bun API + React/Vite client",
    ),
    (
        "ai-chat",
        "examples/ai-chat",
        "Full-stack AI",
        "React chat UI + Bun agent loop via the LLM gateway",
    ),
];

/// A resolved template source.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Copy from a local directory.
    Local(PathBuf),
    /// Fetch from a git repo (shallow clone), optionally a ref and subdir.
    Git {
        repo: String,
        reference: Option<String>,
        subdir: Option<String>,
    },
}

/// The configured default repo (env override or the built-in constant).
fn default_repo() -> String {
    std::env::var("RIZ_TEMPLATE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

/// True if `s` denotes a local filesystem path rather than a remote/shorthand.
fn looks_local(s: &str) -> bool {
    s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
        || s.starts_with('~')
        || s == "."
        || Path::new(s).is_dir()
}

/// True if `s` is a git URL we clone verbatim (incl. `file://` for tests).
fn looks_like_git_url(s: &str) -> bool {
    s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.starts_with("file://")
        || s.ends_with(".git")
}

/// Split a trailing `#ref` off a spec, returning (head, ref?).
fn split_ref(s: &str) -> (&str, Option<String>) {
    match s.split_once('#') {
        Some((head, r)) if !r.is_empty() => (head, Some(r.to_string())),
        _ => (s, None),
    }
}

/// Resolve a `riz init` spec into a [`Source`]. `cli_ref` (from `--ref`)
/// overrides any `#ref` embedded in the spec.
pub fn resolve(spec: &str, cli_ref: Option<&str>) -> anyhow::Result<Source> {
    // 1. Built-in name → a subdir of the default repo.
    if let Some((_, subdir, _, _)) = BUILTINS.iter().find(|(n, ..)| *n == spec) {
        let base = default_repo();
        let reference = cli_ref.map(str::to_string);
        // A local default-repo override (used by tests/forks) → copy locally.
        if looks_local(&base) {
            return Ok(Source::Local(PathBuf::from(base).join(subdir)));
        }
        return Ok(Source::Git {
            repo: base,
            reference,
            subdir: Some((*subdir).to_string()),
        });
    }

    // 2. Local path.
    if looks_local(spec) {
        let p = expand_tilde(spec);
        anyhow::ensure!(
            p.is_dir(),
            "local template path does not exist: {}",
            p.display()
        );
        return Ok(Source::Local(p));
    }

    let (head, embedded_ref) = split_ref(spec);
    let reference = cli_ref.map(str::to_string).or(embedded_ref);

    // 3. GitHub "tree" browser URL: .../<owner>/<repo>/tree/<ref>/<subdir...>
    if let Some(src) = parse_github_tree_url(head) {
        return Ok(merge_ref(src, reference));
    }

    // 4. Full git URL (cloned whole; subdir not inferable from arbitrary URLs).
    if looks_like_git_url(head) {
        return Ok(Source::Git {
            repo: head.to_string(),
            reference,
            subdir: None,
        });
    }

    // 5. Shorthand: owner/repo[/sub/dir] on GitHub.
    if let Some(src) = parse_shorthand(head) {
        return Ok(merge_ref(src, reference));
    }

    anyhow::bail!(
        "could not understand template spec {spec:?}. Use a built-in name \
         (run `riz init --list`), an `owner/repo[/subdir]` spec, a git URL, or a \
         local path."
    )
}

fn merge_ref(src: Source, reference: Option<String>) -> Source {
    match src {
        Source::Git {
            repo,
            reference: inner,
            subdir,
        } => Source::Git {
            repo,
            reference: reference.or(inner),
            subdir,
        },
        other => other,
    }
}

/// `https://github.com/<o>/<r>/tree/<ref>/<subdir...>` → Git source.
///
/// Iterator-driven (no positional indexing): a spec with too few segments
/// falls out at the first missing `next()` and resolves to `None`, which the
/// caller turns into the "could not understand template spec" error.
fn parse_github_tree_url(s: &str) -> Option<Source> {
    let rest = s.strip_prefix("https://github.com/")?;
    // owner / repo / "tree" / ref / subdir...
    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?;
    let repo_name = parts.next()?;
    if parts.next()? != "tree" {
        return None;
    }
    let reference = Some(parts.next()?.to_string());
    let subdir: Vec<&str> = parts.collect();
    let subdir = if subdir.is_empty() {
        None
    } else {
        Some(subdir.join("/"))
    };
    Some(Source::Git {
        repo: format!("https://github.com/{owner}/{repo_name}"),
        reference,
        subdir,
    })
}

/// `owner/repo[/sub/dir]` → GitHub Git source. Iterator-driven like
/// [`parse_github_tree_url`]; malformed shorthands resolve to `None`.
fn parse_shorthand(s: &str) -> Option<Source> {
    let mut parts = s.split('/');
    let owner = parts.next()?;
    let repo_name = parts.next()?;
    if owner.is_empty() || repo_name.is_empty() {
        return None;
    }
    let subdir: Vec<&str> = parts.collect();
    let subdir = if subdir.is_empty() {
        None
    } else {
        Some(subdir.join("/"))
    };
    Some(Source::Git {
        repo: format!("https://github.com/{owner}/{repo_name}"),
        reference: None,
        subdir,
    })
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Fetch `source` into `dest`. Refuses a non-empty `dest` unless `force`.
/// Returns the number of files written.
pub fn fetch_into(source: &Source, dest: &Path, force: bool) -> anyhow::Result<usize> {
    if dest.exists()
        && dest
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
        && !force
    {
        anyhow::bail!(
            "target directory {} is not empty. Move it aside or pass --force.",
            dest.display()
        );
    }

    match source {
        Source::Local(path) => {
            anyhow::ensure!(
                path.is_dir(),
                "template source is not a directory: {}",
                path.display()
            );
            std::fs::create_dir_all(dest)?;
            copy_dir(path, dest)
        }
        Source::Git {
            repo,
            reference,
            subdir,
        } => {
            let tmp = ScratchDir::new()?;
            let clone_dir = tmp.path().join("repo");
            git_clone(repo, reference.as_deref(), &clone_dir)?;
            let src = match subdir {
                Some(sub) => clone_dir.join(sub),
                None => clone_dir.clone(),
            };
            anyhow::ensure!(
                src.is_dir(),
                "subdir {:?} not found in {repo}{}",
                subdir.as_deref().unwrap_or(""),
                reference
                    .as_deref()
                    .map(|r| format!(" @ {r}"))
                    .unwrap_or_default()
            );
            std::fs::create_dir_all(dest)?;
            copy_dir(&src, dest)
        }
    }
}

/// A throwaway scratch directory that removes itself on drop. Avoids pulling
/// the `tempfile` crate into the release binary just for `git clone` staging.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("riz-init-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Shallow `git clone` of `repo` (optionally a branch/tag `reference`) into
/// `dest`. Requires `git` on PATH; surfaces a clear error otherwise.
fn git_clone(repo: &str, reference: Option<&str>, dest: &Path) -> anyhow::Result<()> {
    use std::process::Command;
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1").arg("--quiet");
    if let Some(r) = reference {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(repo).arg(dest);
    let out = cmd.output().map_err(|e| {
        anyhow::anyhow!(
            "could not run `git` to fetch the template ({e}). `riz init` fetches \
             templates from git — install git, or pass a local path."
        )
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "git clone of {repo}{} failed:\n{}",
            reference.map(|r| format!(" (ref {r})")).unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Directory names never carried into a freshly-scaffolded project. A real
/// `git clone` already omits these (they're gitignored); skipping them keeps a
/// LOCAL-path copy behaving identically instead of dragging along a developer's
/// installed deps / build output.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".vite"];

fn is_skipped(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| SKIP_DIRS.contains(&n) || n.ends_with(".tsbuildinfo"))
}

/// Depth cap for the recursive template copy (rule 1: recursion over an
/// external directory tree carries an explicit bound). Real templates are a
/// handful of levels deep; the cap's job is to fail cleanly on pathological
/// trees — most notably a symlinked directory cycle, which `is_dir()`
/// follows. Kept below the OS's own symlink-resolution limit (MAXSYMLINKS,
/// 32 on macOS/Linux) so the actionable message below fires before a raw
/// ELOOP does.
const MAX_COPY_DEPTH: usize = 16;

/// Recursively copy `src` into `dst`, skipping VCS/dep/build cruft. Returns
/// files written. Depth-capped at [`MAX_COPY_DEPTH`].
fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    copy_dir_at(src, dst, 0)
}

fn copy_dir_at(src: &Path, dst: &Path, depth: usize) -> anyhow::Result<usize> {
    if depth >= MAX_COPY_DEPTH {
        anyhow::bail!(
            "template directory nests deeper than {MAX_COPY_DEPTH} levels at {} — \
             refusing to copy (is there a symlink cycle in the template?)",
            src.display()
        );
    }
    let mut count: usize = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_skipped(&name) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            std::fs::create_dir_all(&to)?;
            // Saturating adds: `count` is bounded by the number of files in
            // the template tree; saturation cannot realistically be reached
            // and a clamped count only affects the CLI's summary line.
            count = count.saturating_add(copy_dir_at(&from, &to, depth.saturating_add(1))?);
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_name_resolves_to_repo_subdir() {
        // Clear any test-suite override for this assertion.
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("typescript-http", None).unwrap();
        assert_eq!(
            s,
            Source::Git {
                repo: DEFAULT_REPO.to_string(),
                reference: None,
                subdir: Some("templates/typescript-http".to_string()),
            }
        );
    }

    #[test]
    fn full_stack_builtin_points_at_examples() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("typescript-todo", None).unwrap();
        match s {
            Source::Git { subdir, .. } => {
                assert_eq!(subdir.as_deref(), Some("examples/typescript-todo"))
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn shorthand_owner_repo_subdir_and_ref() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("acme/widgets/templates/api#v2", None).unwrap();
        assert_eq!(
            s,
            Source::Git {
                repo: "https://github.com/acme/widgets".to_string(),
                reference: Some("v2".to_string()),
                subdir: Some("templates/api".to_string()),
            }
        );
    }

    #[test]
    fn cli_ref_overrides_embedded_ref() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("acme/widgets#v1", Some("main")).unwrap();
        match s {
            Source::Git { reference, .. } => assert_eq!(reference.as_deref(), Some("main")),
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn github_tree_url_extracts_ref_and_subdir() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve(
            "https://github.com/acme/widgets/tree/dev/examples/todo",
            None,
        )
        .unwrap();
        assert_eq!(
            s,
            Source::Git {
                repo: "https://github.com/acme/widgets".to_string(),
                reference: Some("dev".to_string()),
                subdir: Some("examples/todo".to_string()),
            }
        );
    }

    #[test]
    fn full_git_url_is_cloned_whole() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("git@github.com:acme/widgets.git", None).unwrap();
        assert_eq!(
            s,
            Source::Git {
                repo: "git@github.com:acme/widgets.git".to_string(),
                reference: None,
                subdir: None,
            }
        );
    }

    #[test]
    fn local_path_resolves_to_local_source() {
        let dir = tempfile::tempdir().unwrap();
        let s = resolve(dir.path().to_str().unwrap(), None).unwrap();
        assert_eq!(s, Source::Local(dir.path().to_path_buf()));
    }

    #[test]
    fn builtin_with_local_default_repo_is_local() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RIZ_TEMPLATE_REPO", dir.path());
        let s = resolve("typescript-http", None).unwrap();
        assert_eq!(
            s,
            Source::Local(dir.path().join("templates/typescript-http"))
        );
        std::env::remove_var("RIZ_TEMPLATE_REPO");
    }

    /// A symlinked directory cycle in a local template must produce a clean
    /// error, not unbounded recursion (rule 1). Whether the depth cap or the
    /// OS's own symlink-resolution limit (ELOOP) fires first depends on how
    /// many symlinks each path component costs — either way the copy stops
    /// with an `Err`, never a hang or a panic.
    #[cfg(unix)]
    #[test]
    fn copy_refuses_symlink_cycle_with_clean_error() {
        let from = tempfile::tempdir().unwrap();
        std::fs::write(from.path().join("riz.toml"), "x").unwrap();
        std::os::unix::fs::symlink(from.path(), from.path().join("loop")).unwrap();

        let to = tempfile::tempdir().unwrap();
        let dest = to.path().join("app");
        let err = fetch_into(&Source::Local(from.path().to_path_buf()), &dest, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nests deeper") || msg.contains("symbolic links"),
            "expected the depth-cap or ELOOP error, got: {msg}"
        );
    }

    /// The depth cap itself, deterministically: a symlink-free tree nested
    /// past MAX_COPY_DEPTH gets the actionable depth-cap message.
    #[test]
    fn copy_deeper_than_cap_gets_depth_cap_error() {
        let from = tempfile::tempdir().unwrap();
        let mut deep = from.path().to_path_buf();
        for i in 0..=MAX_COPY_DEPTH {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf.txt"), "x").unwrap();

        let to = tempfile::tempdir().unwrap();
        let dest = to.path().join("app");
        let err = fetch_into(&Source::Local(from.path().to_path_buf()), &dest, false).unwrap_err();
        assert!(
            err.to_string().contains("nests deeper"),
            "expected the depth-cap error, got: {err}"
        );
    }

    /// A tree URL with too few segments (no ref) is not a tree source; it
    /// falls through to the whole-repo git-URL branch, same as before the
    /// iterator rewrite.
    #[test]
    fn truncated_tree_url_falls_back_to_whole_clone() {
        std::env::remove_var("RIZ_TEMPLATE_REPO");
        let s = resolve("https://github.com/acme/widgets/tree", None).unwrap();
        assert_eq!(
            s,
            Source::Git {
                repo: "https://github.com/acme/widgets/tree".to_string(),
                reference: None,
                subdir: None,
            }
        );
    }

    #[test]
    fn local_copy_and_force_semantics() {
        let from = tempfile::tempdir().unwrap();
        std::fs::write(from.path().join("riz.toml"), "x").unwrap();
        std::fs::create_dir(from.path().join(".git")).unwrap();
        std::fs::write(from.path().join(".git/HEAD"), "ref").unwrap();

        let to = tempfile::tempdir().unwrap();
        let dest = to.path().join("app");
        let n = fetch_into(&Source::Local(from.path().to_path_buf()), &dest, false).unwrap();
        assert_eq!(n, 1, "copies riz.toml but skips .git");
        assert!(dest.join("riz.toml").is_file());
        assert!(!dest.join(".git").exists(), ".git must not be copied");

        // Non-empty dest without --force is refused.
        let err = fetch_into(&Source::Local(from.path().to_path_buf()), &dest, false).unwrap_err();
        assert!(err.to_string().contains("not empty"));
        // With force it succeeds.
        fetch_into(&Source::Local(from.path().to_path_buf()), &dest, true).unwrap();
    }
}
