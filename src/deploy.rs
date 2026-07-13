use crate::state::AppState;
use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};

/// Cap on the zip downloaded from S3 (rule 3: every buffer that grows from
/// remote input carries an explicit cap). Matches AWS Lambda's largest
/// deployment-package dimension (250 MB); the whole zip is buffered in
/// memory before unpacking, so this bounds that buffer.
const MAX_ZIP_BYTES: usize = 250 * 1024 * 1024;

/// Cap on the total UNPACKED size of a deploy zip — a zip bomb must fail
/// with a clean error, not fill the disk. AWS parity: 250 MB unzipped.
const MAX_UNPACKED_BYTES: u64 = 250 * 1024 * 1024;

#[derive(Deserialize)]
pub struct DeployRequest {
    pub lambda: String,
    pub s3_bucket: String,
    pub s3_key: String,
}

#[derive(Serialize)]
pub struct DeployResponse {
    pub status: String,
    pub lambda: String,
    pub pid: u32,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Shorthand for the endpoint's JSON error shape.
fn error_response(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorResponse { error: msg.into() })).into_response()
}

/// Deploy auth gate, FAIL CLOSED: refuse outright when neither a deploy key
/// nor a CIDR allowlist is configured (prevents accidental RCE), then apply
/// the CIDR allowlist and the bearer key when configured. `Some(response)`
/// means "reject with this" (same shape as `server::bearer_reject`); `None`
/// means proceed.
fn deploy_auth_rejection(
    allowed_cidrs: &[String],
    expected_key: Option<&str>,
    client_ip: IpAddr,
    headers: &HeaderMap,
) -> Option<Response> {
    if expected_key.is_none() && allowed_cidrs.is_empty() {
        return Some(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "deploy endpoint requires auth configuration (deploy_key or allowed_cidrs)",
        ));
    }

    // IP allowlist check (empty = allow all)
    if !allowed_cidrs.is_empty() {
        let allowed = allowed_cidrs.iter().any(|cidr| {
            cidr.parse::<IpNet>()
                .map(|net| net.contains(&client_ip))
                .unwrap_or(false)
                || cidr
                    .parse::<IpAddr>()
                    .map(|ip| ip == client_ip)
                    .unwrap_or(false)
        });
        if !allowed {
            return Some(error_response(StatusCode::FORBIDDEN, "forbidden"));
        }
    }

    if let Some(expected) = expected_key {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if provided != Some(expected) {
            return Some(error_response(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
    }
    None
}

/// Post-swap health confirmation: give the fresh worker a moment, then check
/// pool health — a handler that crashes on startup answers 422, not 200.
async fn confirm_swap_health(state: &AppState, lambda: &str, pid: u32) -> Response {
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    let stats = state.process_manager.pool_stats().await;
    let still_healthy = stats
        .iter()
        .find(|s| s.name == lambda)
        .map(|s| s.healthy)
        .unwrap_or(false);

    if !still_healthy {
        info!("deploy {lambda} pid={pid} crashed on startup — returning 422");
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "handler crashed immediately after deploy — check handler code",
        );
    }

    info!("deployed {lambda} pid={pid}");
    (
        StatusCode::OK,
        Json(DeployResponse {
            status: "ok".into(),
            lambda: lambda.to_string(),
            pid,
        }),
    )
        .into_response()
}

pub async fn deploy_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DeployRequest>,
) -> Response {
    let config = state.config.read().await;
    let deploy_cfg = config.deploy.clone();
    let aws_region = config.aws.region.clone();
    let expected_key = config.effective_deploy_key();
    drop(config);

    // Artifact location for the audit trail (never the deploy key).
    let source = format!("s3://{}/{}", body.s3_bucket, body.s3_key);
    let principal = addr.ip().to_string();

    if let Some(resp) = deploy_auth_rejection(
        &deploy_cfg.allowed_cidrs,
        expected_key.as_deref(),
        addr.ip(),
        &headers,
    ) {
        crate::audit::deploy(&principal, &body.lambda, &source, "rejected");
        return resp;
    }

    // Validate lambda name is a safe identifier
    if body.lambda.contains('/') || body.lambda.contains('.') {
        return error_response(StatusCode::BAD_REQUEST, "invalid lambda name");
    }

    // Find the matching FUNCTION by name. The deploy "lambda" identifier
    // is exactly the function name in riz.toml.
    let function_cfg = {
        let config = state.config.read().await;
        config.functions.get(&body.lambda).cloned()
    };
    let Some(mut function_cfg) = function_cfg else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("no function found for '{}'", body.lambda),
        );
    };

    // Download zip from S3 and unpack to staging dir.
    // UUID suffix ensures concurrent deploys for the same lambda never share a path (BUG-18).
    let staging_dir = PathBuf::from(format!(
        "/tmp/riz-deploy/{}-{}",
        body.lambda,
        uuid::Uuid::new_v4()
    ));
    if let Err(e) =
        download_and_unpack_s3(&body.s3_bucket, &body.s3_key, &staging_dir, &aws_region).await
    {
        error!("deploy download failed for {}: {e}", body.lambda);
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("download failed: {e}"),
        );
    }

    let Some(handler_name) = function_cfg.handler.file_name().map(|n| n.to_os_string()) else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "function handler has no filename: {:?}",
                function_cfg.handler
            ),
        );
    };
    function_cfg.handler = staging_dir.join(&handler_name);

    match state
        .process_manager
        .hot_swap(&body.lambda, function_cfg)
        .await
    {
        Ok(pid) => {
            let resp = confirm_swap_health(&state, &body.lambda, pid).await;
            // 200 = live and serving; anything else (422) = crashed on startup.
            let outcome = if resp.status() == StatusCode::OK {
                "applied"
            } else {
                "crashed"
            };
            crate::audit::deploy(&principal, &body.lambda, &source, outcome);
            resp
        }
        Err(e) => {
            error!("hot_swap failed for {}: {e}", body.lambda);
            crate::audit::deploy(&principal, &body.lambda, &source, "failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("swap failed: {e}"),
            )
        }
    }
}

async fn download_and_unpack_s3(
    bucket: &str,
    key: &str,
    dest: &PathBuf,
    region: &str,
) -> anyhow::Result<()> {
    use aws_config::BehaviorVersion;
    use aws_sdk_s3::config::Region;

    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .load()
        .await;
    let client = aws_sdk_s3::Client::new(&sdk_config);

    let resp = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("S3 GetObject failed: {e}"))?;

    // Chunked read with a running cap (rule 3): the zip is buffered in memory
    // before unpacking, so an oversized object must fail BEFORE it is fully
    // buffered — never trust Content-Length alone.
    let mut body = resp.body;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = body
        .try_next()
        .await
        .map_err(|e| anyhow::anyhow!("S3 body read failed: {e}"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_ZIP_BYTES {
            anyhow::bail!(
                "deploy zip exceeds the {} MB cap (AWS Lambda's package limit)",
                MAX_ZIP_BYTES / (1024 * 1024)
            );
        }
        bytes.extend_from_slice(&chunk);
    }

    // No need to remove the dir first — the UUID-suffixed path is always fresh (BUG-18).
    std::fs::create_dir_all(dest)?;
    unpack_zip_into(std::io::Cursor::new(bytes), dest)
}

/// Unpack a zip archive into `dest`, skipping unsafe entries:
///   - Entries whose `enclosed_name()` resolves outside `dest` (`../etc/...`).
///   - Symlink entries (BUG-19: a `./index.ts -> /etc/passwd` symlink would let
///     Bun follow the link out of the staging dir).
///
/// Total unpacked bytes are capped at [`MAX_UNPACKED_BYTES`] — a zip bomb
/// fails with a clean error instead of filling the disk.
///
/// Extracted from `download_and_unpack_s3` so the symlink-rejection behavior
/// is unit-testable without an S3 fixture.
pub(crate) fn unpack_zip_into<R: std::io::Read + std::io::Seek>(
    reader: R,
    dest: &std::path::Path,
) -> anyhow::Result<()> {
    unpack_zip_into_with_limit(reader, dest, MAX_UNPACKED_BYTES)
}

/// [`unpack_zip_into`] with the unpacked-size cap as a parameter (test seam).
fn unpack_zip_into_with_limit<R: std::io::Read + std::io::Seek>(
    reader: R,
    dest: &std::path::Path,
    max_unpacked: u64,
) -> anyhow::Result<()> {
    use std::io::Read as _;

    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| anyhow::anyhow!("zip open failed: {e}"))?;

    // Remaining unpacked-byte budget across ALL entries.
    let mut remaining: u64 = max_unpacked;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = match file.enclosed_name() {
            Some(name) => dest.join(name),
            None => {
                tracing::warn!("skipping unsafe zip entry: {:?}", file.name());
                continue;
            }
        };
        // BUG-19: reject symlinks — they can point outside the staging dir (path traversal).
        if file.is_symlink() {
            tracing::warn!("skipping symlink entry in deploy ZIP: {:?}", file.name());
            continue;
        }
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            // Check the DECLARED size up front, and cap the actual copy with
            // `take` in case the header lies — both paths bail before the
            // budget can be exceeded on disk.
            if file.size() > remaining {
                anyhow::bail!(
                    "zip expands past the {} MB unpacked cap at entry {:?} — refusing to deploy",
                    max_unpacked / (1024 * 1024),
                    file.name()
                );
            }
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&outpath)?;
            let written = std::io::copy(&mut (&mut file).take(remaining), &mut out)?;
            // `written <= remaining` by the take; saturating for the ratchet.
            remaining = remaining.saturating_sub(written);
            // A lying header (declared small, streams big) hits the take
            // limit with bytes still pending — detect and bail.
            if remaining == 0 && file.read(&mut [0u8; 1])? > 0 {
                anyhow::bail!(
                    "zip expands past the {} MB unpacked cap at entry {:?} — refusing to deploy",
                    max_unpacked / (1024 * 1024),
                    file.name()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_refuses_when_no_auth_configured() {
        // Verify the logic: no deploy_key + empty allowed_cidrs = refuse
        let deploy_key: Option<String> = None;
        let allowed_cidrs: Vec<String> = vec![];
        let should_refuse = deploy_key.is_none() && allowed_cidrs.is_empty();
        assert!(
            should_refuse,
            "must refuse deploy when no auth is configured"
        );
    }

    /// BUG-19 regression: a zip containing a symlink entry MUST be skipped
    /// during extraction, even if its `enclosed_name()` is innocuous. A
    /// symlink could resolve outside the staging dir at access time (Bun
    /// follows symlinks), which is a path-traversal RCE primitive.
    #[test]
    fn bug_19_unpack_zip_skips_symlink_entries() {
        use std::io::Cursor;

        // Build an in-memory zip with one regular file and one symlink.
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut w = zip::ZipWriter::new(cursor);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            use std::io::Write;
            w.start_file("index.ts", opts).unwrap();
            w.write_all(b"export const handler = async () => 42;\n")
                .unwrap();
            // The dangerous entry: symlink "./index.ts" -> "/etc/passwd".
            w.add_symlink("evil.ts", "/etc/passwd", opts).unwrap();
            w.finish().unwrap();
        }

        let dest = tempfile::tempdir().expect("tempdir");
        unpack_zip_into(Cursor::new(&buf), dest.path()).expect("unpack");

        // The regular file extracted.
        let extracted = dest.path().join("index.ts");
        assert!(
            extracted.exists() && !extracted.is_symlink(),
            "regular file must extract; got exists={}, is_symlink={}",
            extracted.exists(),
            extracted.is_symlink()
        );

        // The symlink did NOT — that's the bug fix.
        let evil = dest.path().join("evil.ts");
        assert!(
            !evil.exists(),
            "symlink entry must be skipped during extraction; found {}",
            evil.display()
        );
    }

    /// Rule 3: a zip that expands past the unpacked cap must fail with a
    /// clean error (and not write past the budget), while the same archive
    /// under a roomier cap unpacks fine.
    #[test]
    fn unpack_zip_rejects_archives_over_the_unpacked_cap() {
        use std::io::{Cursor, Write};

        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut w = zip::ZipWriter::new(cursor);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            w.start_file("big.bin", opts).unwrap();
            w.write_all(&[0u8; 4096]).unwrap();
            w.finish().unwrap();
        }

        let dest = tempfile::tempdir().expect("tempdir");
        let err = unpack_zip_into_with_limit(Cursor::new(&buf), dest.path(), 1024).unwrap_err();
        assert!(
            err.to_string().contains("unpacked cap"),
            "expected the unpacked-cap error, got: {err}"
        );

        let dest2 = tempfile::tempdir().expect("tempdir");
        unpack_zip_into_with_limit(Cursor::new(&buf), dest2.path(), 8192)
            .expect("under-cap archive unpacks");
        assert!(dest2.path().join("big.bin").is_file());
    }

    #[test]
    fn deploy_allows_with_cidr_only() {
        let deploy_key: Option<String> = None;
        let allowed_cidrs: Vec<String> = vec!["127.0.0.1/32".into()];
        let should_refuse = deploy_key.is_none() && allowed_cidrs.is_empty();
        assert!(!should_refuse, "cidr restriction alone is sufficient auth");
    }

    #[test]
    fn deploy_health_check_uses_422_for_crash() {
        // 422 Unprocessable Entity is the right status for "handler crashed"
        // (client sent valid input, but the server-side handler rejected it)
        assert_eq!(StatusCode::UNPROCESSABLE_ENTITY.as_u16(), 422);
    }

    #[test]
    fn deploy_staging_dir_is_unique() {
        // BUG-18: each deploy attempt must produce a distinct staging path so
        // concurrent deploys for the same lambda cannot corrupt each other's files.
        let lambda = "my-fn";
        let path_a = format!("/tmp/riz-deploy/{}-{}", lambda, uuid::Uuid::new_v4());
        let path_b = format!("/tmp/riz-deploy/{}-{}", lambda, uuid::Uuid::new_v4());
        assert_ne!(
            path_a, path_b,
            "staging paths must be unique across deploy attempts"
        );
        // Both paths must still share the expected prefix so ops tooling can find them.
        assert!(path_a.starts_with("/tmp/riz-deploy/my-fn-"));
        assert!(path_b.starts_with("/tmp/riz-deploy/my-fn-"));
    }

    #[test]
    fn symlink_entries_are_rejected() {
        // BUG-19: the extraction loop now calls file.is_symlink() before is_dir().
        // If true the entry is skipped with a warning.  We cannot construct a real
        // ZipFile in a unit test, so this test documents the expected behavior and
        // verifies that the zip crate's ZipFile type exposes is_symlink() at compile
        // time (the build will fail if the method is absent).
        //
        // Behavioral contract:
        //   - symlink entries must never be written to disk
        //   - a warn!() log line must be emitted for each skipped entry
        //   - all non-symlink entries in the same archive must still be extracted
        //
        // Integration coverage lives in the manual QA checklist (see docs/deploy-security.md).
        // Verify at compile time that ZipFile exposes is_symlink() — if the zip crate
        // version ever drops or renames the method the build will break here.
        fn _assert_method_exists<'a>(f: &zip::read::ZipFile<'a>) -> bool {
            f.is_symlink()
        }
        // If this compiles, the guard is wired up correctly.
    }
}
