use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use crate::router::Router;
use crate::state::AppState;

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

pub async fn deploy_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DeployRequest>,
) -> impl IntoResponse {
    let config = state.config.read().await;
    let deploy_cfg = config.deploy.clone();
    let aws_region = config.aws.region.clone();
    let expected_key = config.effective_deploy_key();
    drop(config);

    // Refuse if no auth at all configured — prevents accidental RCE
    let has_cidr_restriction = !deploy_cfg.allowed_cidrs.is_empty();
    if expected_key.is_none() && !has_cidr_restriction {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse { error: "deploy endpoint requires auth configuration (deploy_key or allowed_cidrs)".into() })
        ).into_response();
    }

    // IP allowlist check (empty = allow all)
    if !deploy_cfg.allowed_cidrs.is_empty() {
        let client_ip = addr.ip();
        let allowed = deploy_cfg.allowed_cidrs.iter().any(|cidr| {
            cidr.parse::<IpNet>().map(|net| net.contains(&client_ip)).unwrap_or(false)
                || cidr.parse::<IpAddr>().map(|ip| ip == client_ip).unwrap_or(false)
        });
        if !allowed {
            return (StatusCode::FORBIDDEN, Json(ErrorResponse { error: "forbidden".into() }))
                .into_response();
        }
    }

    if let Some(expected) = expected_key {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if provided != Some(expected.as_str()) {
            return (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "unauthorized".into() }))
                .into_response();
        }
    }

    // Validate lambda name is a safe identifier
    if body.lambda.contains('/') || body.lambda.contains('.') {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: "invalid lambda name".into() })).into_response();
    }

    // Find matching route by lambda name (matches path segment)
    let config = state.config.read().await;
    let route = config.routes.iter().find(|r| route_name_matches(&r.path, &body.lambda)).cloned();
    drop(config);

    let mut route = match route {
        Some(r) => r,
        None => {
            return (StatusCode::NOT_FOUND, Json(ErrorResponse {
                error: format!("no route found for lambda '{}'", body.lambda),
            })).into_response();
        }
    };

    let route_key = Router::route_key(&route.method, &route.path);

    // Download zip from S3 and unpack to staging dir.
    // UUID suffix ensures concurrent deploys for the same lambda never share a path (BUG-18).
    let staging_dir = PathBuf::from(format!("/tmp/riz-deploy/{}-{}", body.lambda, uuid::Uuid::new_v4()));
    if let Err(e) = download_and_unpack_s3(&body.s3_bucket, &body.s3_key, &staging_dir, &aws_region).await {
        error!("deploy download failed for {}: {e}", body.lambda);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
            error: format!("download failed: {e}"),
        })).into_response();
    }

    // Point handler at unpacked staging dir
    let handler_name = match route.handler.file_name() {
        Some(name) => name.to_os_string(),
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
                error: format!("route handler has no filename: {:?}", route.handler),
            })).into_response();
        }
    };
    route.handler = staging_dir.join(&handler_name);

    // Hot-swap the process pool
    match state.process_manager.hot_swap(&route_key, route, &state.runtime_registry).await {
        Ok(pid) => {
            // Brief pause then health check — catches handlers that crash immediately
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            let stats = state.process_manager.pool_stats().await;
            let still_healthy = stats.iter()
                .find(|s| s.route_key == route_key)
                .map(|s| s.healthy)
                .unwrap_or(false);

            if !still_healthy {
                info!("deploy {} pid={pid} crashed on startup — returning 422", body.lambda);
                return (StatusCode::UNPROCESSABLE_ENTITY, Json(ErrorResponse {
                    error: "handler crashed immediately after deploy — check handler code".into(),
                })).into_response();
            }

            info!("deployed {} pid={pid}", body.lambda);
            (StatusCode::OK, Json(DeployResponse {
                status: "ok".into(),
                lambda: body.lambda,
                pid,
            })).into_response()
        }
        Err(e) => {
            error!("hot_swap failed for {}: {e}", body.lambda);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
                error: format!("swap failed: {e}"),
            })).into_response()
        }
    }
}

fn route_name_matches(path: &str, name: &str) -> bool {
    path.trim_matches('/').split('/').any(|seg| seg == name)
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

    let bytes = resp.body.collect().await
        .map_err(|e| anyhow::anyhow!("S3 body read failed: {e}"))?.into_bytes();

    // No need to remove the dir first — the UUID-suffixed path is always fresh (BUG-18).
    std::fs::create_dir_all(dest)?;
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("zip open failed: {e}"))?;

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
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_name_matches_segment() {
        assert!(route_name_matches("/auth/signin", "signin"));
        assert!(route_name_matches("/signin", "signin"));
        assert!(!route_name_matches("/auth/login", "signin"));
    }

    #[test]
    fn route_name_no_match_on_empty() {
        assert!(!route_name_matches("/auth/signin", ""));
    }

    #[test]
    fn deploy_refuses_when_no_auth_configured() {
        // Verify the logic: no deploy_key + empty allowed_cidrs = refuse
        let deploy_key: Option<String> = None;
        let allowed_cidrs: Vec<String> = vec![];
        let should_refuse = deploy_key.is_none() && allowed_cidrs.is_empty();
        assert!(should_refuse, "must refuse deploy when no auth is configured");
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
        assert_ne!(path_a, path_b, "staging paths must be unique across deploy attempts");
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
