//! Static-file serving — the fallback after API routes (see
//! `docs/superpowers/specs/2026-06-18-static-serving-design.md`).
//!
//! `serve()` is consulted from `dispatch_lambda` ONLY for GET/HEAD requests
//! whose path is owned by no function and isn't `/_riz/*` — so functions and
//! system endpoints always win and a static file can never shadow an API.
//!
//! Hand-rolled (no `tower-http`) to keep the binary lean and to own the
//! precedence, dotfile allow-list, hash-named cache policy, and the
//! agent-discovery files directly. Covers the correctness tail the spec
//! requires: content-type, ETag + conditional requests, single-range, cache
//! headers, directory→index, SPA fallback, and traversal/dotfile safety.

use crate::config::StaticConfig;
use axum::body::Body;
use axum::response::Response;
use http::{HeaderMap, Method, StatusCode};
use std::path::{Path, PathBuf};

/// Serve a static file for a GET/HEAD request, or a static 404. Returns `None`
/// only when the path is not under the configured mount (caller falls through
/// to the normal API 404); otherwise the static layer OWNS the response
/// (file / index / SPA fallback / custom-or-plain 404), so a website miss
/// never leaks the API's JSON 404.
pub async fn serve(
    method: &Method,
    url_path: &str,
    headers: &HeaderMap,
    cfg: &StaticConfig,
) -> Option<Response> {
    // /_riz/* is never served from disk, even if such a file exists.
    if url_path == "/_riz" || url_path.starts_with("/_riz/") {
        return None;
    }
    // Path must be under the mount; strip the mount prefix to get the request
    // path relative to the site root.
    let rel = relative_to_mount(url_path, &cfg.mount)?;

    let is_head = method == Method::HEAD;

    // Resolve to a safe path inside dir. A traversal / bad-dotfile attempt is
    // treated as a normal miss (404), never an error or an escape.
    match resolve(&cfg.dir, rel) {
        Resolved::File(path) => Some(file_response(&path, headers, cfg, is_head).await),
        Resolved::Dir(path) => {
            // Directory request → its index file, else 404.
            let index = path.join(&cfg.index);
            if index.is_file() {
                Some(file_response(&index, headers, cfg, is_head).await)
            } else {
                Some(not_found(cfg, is_head).await)
            }
        }
        Resolved::Missing => {
            // SPA history-API fallback: an extensionless GET that accepts HTML
            // serves index so client-side routes resolve. A missing *asset*
            // (path with an extension) still 404s.
            if cfg.spa_fallback
                && method == Method::GET
                && accepts_html(headers)
                && !has_extension(rel)
            {
                let index = cfg.dir.join(&cfg.index);
                if index.is_file() {
                    return Some(file_response(&index, headers, cfg, false).await);
                }
            }
            Some(not_found(cfg, is_head).await)
        }
        Resolved::Forbidden => Some(not_found(cfg, is_head).await),
    }
}

/// `url_path` relative to `mount`, or `None` if not under it. The returned
/// string never has a leading `/`.
fn relative_to_mount<'a>(url_path: &'a str, mount: &str) -> Option<&'a str> {
    if mount == "/" {
        return Some(url_path.trim_start_matches('/'));
    }
    let m = mount.trim_end_matches('/');
    if url_path == m {
        return Some("");
    }
    url_path.strip_prefix(m).and_then(|r| r.strip_prefix('/'))
}

enum Resolved {
    File(PathBuf),
    Dir(PathBuf),
    Missing,
    /// Traversal / disallowed dotfile — handled as a miss but flagged distinctly.
    Forbidden,
}

/// Join `rel` (a percent-encoded URL path) onto `dir` with strict safety:
/// percent-decode each segment, reject `..`/`.`/empty-after-decode/NUL/embedded
/// separators, hide dotfiles EXCEPT `.well-known`, then canonicalize and assert
/// the result stays inside `dir`.
fn resolve(dir: &Path, rel: &str) -> Resolved {
    let base = match dir.canonicalize() {
        Ok(b) => b,
        Err(_) => return Resolved::Missing,
    };
    let mut out = base.clone();
    if !rel.is_empty() {
        for raw in rel.split('/') {
            if raw.is_empty() {
                continue; // collapse double slashes / trailing slash
            }
            let seg = match percent_decode(raw) {
                Some(s) => s,
                None => return Resolved::Forbidden,
            };
            if seg == "." || seg == ".." || seg.contains('/') || seg.contains('\0') {
                return Resolved::Forbidden;
            }
            // Dotfiles hidden, except the agent-discovery `.well-known` dir.
            if seg.starts_with('.') && seg != ".well-known" {
                return Resolved::Forbidden;
            }
            out.push(seg);
        }
    }

    // Canonicalize the existing target and confirm containment (defeats symlink
    // escape too). A non-existent path won't canonicalize → treat as missing,
    // but still verify each lexical component contained nothing unsafe (done
    // above), so a missing path can't be a traversal.
    match out.canonicalize() {
        Ok(real) => {
            if !real.starts_with(&base) {
                return Resolved::Forbidden;
            }
            if real.is_dir() {
                Resolved::Dir(real)
            } else if real.is_file() {
                Resolved::File(real)
            } else {
                Resolved::Missing
            }
        }
        Err(_) => Resolved::Missing,
    }
}

/// Minimal, strict percent-decoder: returns `None` on malformed `%`.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let h = bytes.get(i + 1)?;
                let l = bytes.get(i + 2)?;
                let hi = (*h as char).to_digit(16)?;
                let lo = (*l as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

async fn file_response(
    path: &Path,
    req_headers: &HeaderMap,
    cfg: &StaticConfig,
    is_head: bool,
) -> Response {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => return not_found(cfg, is_head).await,
    };
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{len:x}-{mtime:x}\"");
    let ctype = content_type(path);
    let cache = cache_control(path, &ctype, cfg);

    // Conditional request → 304 (no body), keeping validators.
    if not_modified(req_headers, &etag) {
        return build(StatusCode::NOT_MODIFIED, &ctype, &cache, &etag, mtime)
            .header(http::header::CONTENT_LENGTH, "0")
            .body(Body::empty())
            .unwrap();
    }

    // Precompressed sibling (path.br / path.gz) when allowed.
    let (read_path, encoding) = pick_encoding(path, req_headers, cfg).await;
    let bytes = match tokio::fs::read(&read_path).await {
        Ok(b) => b,
        Err(_) => return not_found(cfg, is_head).await,
    };
    let total = bytes.len() as u64;

    // Single-range request (Range: bytes=a-b). Only on the identity encoding —
    // ranging a precompressed body would be wrong.
    if encoding.is_none() {
        if let Some(range) = req_headers
            .get(http::header::RANGE)
            .and_then(|v| v.to_str().ok())
        {
            match parse_single_range(range, total) {
                Some((start, end)) => {
                    let slice = bytes[start as usize..=end as usize].to_vec();
                    let b = build(StatusCode::PARTIAL_CONTENT, &ctype, &cache, &etag, mtime)
                        .header(http::header::ACCEPT_RANGES, "bytes")
                        .header(
                            http::header::CONTENT_RANGE,
                            format!("bytes {start}-{end}/{total}"),
                        )
                        .header(http::header::CONTENT_LENGTH, (end - start + 1).to_string());
                    if is_head {
                        return b.body(Body::empty()).unwrap();
                    }
                    return b.body(Body::from(slice)).unwrap();
                }
                None => {
                    return build(
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        &ctype,
                        &cache,
                        &etag,
                        mtime,
                    )
                    .header(http::header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .unwrap();
                }
            }
        }
    }

    let mut b = build(StatusCode::OK, &ctype, &cache, &etag, mtime)
        .header(http::header::ACCEPT_RANGES, "bytes")
        .header(http::header::CONTENT_LENGTH, total.to_string());
    if let Some(enc) = encoding {
        b = b
            .header(http::header::CONTENT_ENCODING, enc)
            .header(http::header::VARY, "Accept-Encoding");
    }
    if is_head {
        return b.body(Body::empty()).unwrap();
    }
    b.body(Body::from(bytes)).unwrap()
}

fn build(
    status: StatusCode,
    ctype: &str,
    cache: &str,
    etag: &str,
    mtime: u64,
) -> http::response::Builder {
    let mut b = Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, ctype)
        .header(http::header::CACHE_CONTROL, cache)
        .header(http::header::ETAG, etag);
    if mtime > 0 {
        b = b.header(http::header::LAST_MODIFIED, httpdate(mtime));
    }
    b
}

async fn not_found(cfg: &StaticConfig, is_head: bool) -> Response {
    if !cfg.not_found.is_empty() {
        let p = cfg.dir.join(&cfg.not_found);
        if let Ok(bytes) = tokio::fs::read(&p).await {
            let b = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(http::header::CONTENT_TYPE, content_type(&p))
                .header(http::header::CACHE_CONTROL, &cfg.cache_html);
            if is_head {
                return b.body(Body::empty()).unwrap();
            }
            return b.body(Body::from(bytes)).unwrap();
        }
    }
    let body = if is_head {
        Body::empty()
    } else {
        Body::from("404 not found")
    };
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .unwrap()
}

fn not_modified(req: &HeaderMap, etag: &str) -> bool {
    if let Some(inm) = req
        .get(http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if inm == "*" || inm.split(',').any(|t| t.trim() == etag) {
            return true;
        }
    }
    false
}

/// Pick `path.br` / `path.gz` if `precompressed` and the client accepts it.
async fn pick_encoding(
    path: &Path,
    req: &HeaderMap,
    cfg: &StaticConfig,
) -> (PathBuf, Option<&'static str>) {
    if !cfg.precompressed {
        return (path.to_path_buf(), None);
    }
    let ae = req
        .get(http::header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ae.contains("br") {
        let p = append_ext(path, "br");
        if tokio::fs::metadata(&p).await.is_ok() {
            return (p, Some("br"));
        }
    }
    if ae.contains("gzip") {
        let p = append_ext(path, "gz");
        if tokio::fs::metadata(&p).await.is_ok() {
            return (p, Some("gzip"));
        }
    }
    (path.to_path_buf(), None)
}

fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Cache-Control: HTML revalidates; hash-named assets are immutable; the rest
/// gets the asset default.
fn cache_control(path: &Path, ctype: &str, cfg: &StaticConfig) -> String {
    if ctype.starts_with("text/html") {
        return cfg.cache_html.clone();
    }
    if is_hash_named(path) {
        return cfg.cache_immutable.clone();
    }
    cfg.cache_assets.clone()
}

/// True when the filename carries a content hash, the shape bundlers emit for
/// fingerprinted assets. Covers both common conventions:
///   - `app.4f1c2a9b.js` — hash as a dot-separated segment.
///   - `index-D5qCqGHz.js` — hash appended with a dash (Vite, webpack, esbuild).
fn is_hash_named(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    // The hash lives in the file stem (must have an extension).
    let stem = match name.rsplit_once('.') {
        Some((stem, _ext)) => stem,
        None => return false,
    };
    // The hash is the last token of the stem under the separators bundlers use.
    let token = stem.rsplit(['.', '-']).next().unwrap_or(stem);
    is_content_hash(token)
}

/// A content-hash token: 8+ alphanumerics that look *generated* rather than a
/// word — they contain a digit or mix upper- and lower-case. This keeps a real
/// name like `main-component.js` on the normal cache while `index-D5qCqGHz.js`
/// and `app.4f1c2a9b.js` get the immutable 1-year cache.
fn is_content_hash(s: &str) -> bool {
    if s.len() < 8 || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    has_digit || (has_upper && has_lower)
}

fn has_extension(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .map(|seg| seg.contains('.'))
        .unwrap_or(false)
}

fn accepts_html(req: &HeaderMap) -> bool {
    req.get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("text/html") || a.contains("*/*"))
        .unwrap_or(false)
}

/// Parse a single `bytes=start-end` range. Returns the inclusive (start, end)
/// or `None` if malformed/unsatisfiable. Only one range is supported.
fn parse_single_range(header: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multi-range unsupported → treat as unsatisfiable
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", "") => return None,
        ("", n) => {
            // suffix: last N bytes
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (total.saturating_sub(n), total - 1)
        }
        (s, "") => (s.parse().ok()?, total - 1),
        (s, e) => (s.parse().ok()?, e.parse().ok()?),
    };
    if start > end || end >= total {
        return None;
    }
    Some((start, end))
}

/// Extension → Content-Type. Dependency-free; covers the web set + the types
/// the spec calls out (wasm, json, svg, txt, webmanifest). Text types carry a
/// utf-8 charset.
fn content_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let (mime, text) = match ext.as_str() {
        "html" | "htm" => ("text/html", true),
        "css" => ("text/css", true),
        "js" | "mjs" => ("text/javascript", true),
        "json" => ("application/json", true),
        "map" => ("application/json", true),
        "svg" => ("image/svg+xml", true),
        "xml" => ("application/xml", true),
        "txt" => ("text/plain", true),
        "md" => ("text/markdown", true),
        "csv" => ("text/csv", true),
        "webmanifest" | "manifest" => ("application/manifest+json", true),
        "wasm" => ("application/wasm", false),
        "png" => ("image/png", false),
        "jpg" | "jpeg" => ("image/jpeg", false),
        "gif" => ("image/gif", false),
        "webp" => ("image/webp", false),
        "avif" => ("image/avif", false),
        "ico" => ("image/x-icon", false),
        "woff" => ("font/woff", false),
        "woff2" => ("font/woff2", false),
        "ttf" => ("font/ttf", false),
        "otf" => ("font/otf", false),
        "pdf" => ("application/pdf", false),
        "mp4" => ("video/mp4", false),
        "webm" => ("video/webm", false),
        "mp3" => ("audio/mpeg", false),
        "wav" => ("audio/wav", false),
        _ => ("application/octet-stream", false),
    };
    if text {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    }
}

/// RFC 7231 IMF-fixdate from a unix timestamp (for `Last-Modified`).
fn httpdate(secs: u64) -> String {
    // days since epoch → civil date (Howard Hinnant's algorithm)
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let dow = ((days % 7 + 4) % 7 + 7) % 7; // 1970-01-01 = Thursday(4)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    const DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DOW[dow as usize],
        d,
        MON[(m - 1) as usize],
        year,
        h,
        mi,
        s
    )
}
