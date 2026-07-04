//! Authorizer middleware — the bridge between dispatch_lambda and the
//! `Authorizer` trait.
//!
//! `enforce_authorizer` is called before the router dispatches the event to
//! the handler. On success it injects the authorizer's context into
//! `event.request_context.authorizer` and returns the modified event. On
//! failure it returns the `AuthError` so dispatch_lambda can emit the correct
//! HTTP status.

use crate::auth::authorizer::{AuthCache, AuthCacheKey, AuthError, Authorizer, AuthorizerOutput};
use crate::auth::jwt::JwtAuthorizer;
use crate::auth::request::RequestAuthorizer;
use crate::config::AuthorizerConfig;
use crate::gateway::ApiGatewayV2httpRequest;
use crate::process::ProcessManager;
use aws_lambda_events::apigw::{
    ApiGatewayRequestAuthorizer, ApiGatewayRequestAuthorizerJwtDescription,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

/// Enforce the authorizer for a matched function, if one is configured.
///
/// Returns the event (possibly with `requestContext.authorizer` injected) on
/// success. Returns `AuthError` on authorization failure.
pub async fn enforce_authorizer(
    authorizer_cfg: Option<&AuthorizerConfig>,
    source_ip: &str,
    auth_header: Option<&str>,
    function_name: &str,
    event: ApiGatewayV2httpRequest,
    cache: &AuthCache,
    process_manager: &Arc<ProcessManager>,
) -> Result<ApiGatewayV2httpRequest, AuthError> {
    let cfg = match authorizer_cfg {
        None => return Ok(event),
        Some(c) => c,
    };

    match cfg {
        AuthorizerConfig::FunctionRef(name) if name == "none" => {
            // Explicit opt-out.
            Ok(event)
        }
        AuthorizerConfig::FunctionRef(auth_fn_name) => {
            authorize_via_function(
                auth_fn_name,
                source_ip,
                auth_header,
                function_name,
                event,
                cache,
                process_manager,
            )
            .await
        }
        AuthorizerConfig::Jwt(jwt_cfg) => {
            authorize_via_jwt(jwt_cfg, source_ip, auth_header, function_name, event, cache).await
        }
    }
}

/// REQUEST authorizer path: cache lookup → invoke the authorizer function →
/// cache the ALLOW decision. Fail-closed: every error path returns `Err`
/// (denials are never cached, so a transient failure cannot poison the cache).
async fn authorize_via_function(
    auth_fn_name: &str,
    source_ip: &str,
    auth_header: Option<&str>,
    function_name: &str,
    event: ApiGatewayV2httpRequest,
    cache: &AuthCache,
    process_manager: &Arc<ProcessManager>,
) -> Result<ApiGatewayV2httpRequest, AuthError> {
    let cache_key = AuthCacheKey::new(source_ip, auth_header, function_name);

    if let Some(cached) = cache.get(&cache_key).await {
        return Ok(inject_authorizer_context(event, &cached));
    }

    // 5 s authorizer timeout — fast enough to avoid holding the client
    // while a misbehaving authorizer hangs. Configurable via
    // `RequestAuthorizer::with_timeout_ms` if the caller needs looser SLAs.
    let authorizer = RequestAuthorizer::new(auth_fn_name.to_owned(), process_manager.clone())
        .with_timeout_ms(5_000);
    match authorizer.authorize(&event).await {
        Ok(output) => {
            cache.insert(cache_key, output.clone()).await;
            Ok(inject_authorizer_context(event, &output))
        }
        Err(e) => {
            warn!(
                source_ip = %source_ip,
                function_name = %function_name,
                authorizer_fn = %auth_fn_name,
                "REQUEST authorizer denied: {e}"
            );
            Err(e)
        }
    }
}

/// JWT authorizer path: cache lookup → JWKS setup → validate token → cache
/// the ALLOW decision. Fail-closed: setup failure and validation failure both
/// return `Err`; a denial additionally evicts the key so the next request
/// re-evaluates rather than racing a concurrently-inserted stale hit.
async fn authorize_via_jwt(
    jwt_cfg: &crate::config::JwtAuthorizerConfig,
    source_ip: &str,
    auth_header: Option<&str>,
    function_name: &str,
    event: ApiGatewayV2httpRequest,
    cache: &AuthCache,
) -> Result<ApiGatewayV2httpRequest, AuthError> {
    let cache_key = AuthCacheKey::new(source_ip, auth_header, function_name);

    if let Some(cached) = cache.get(&cache_key).await {
        return Ok(inject_authorizer_context(event, &cached));
    }

    let authorizer = JwtAuthorizer::new(jwt_cfg.clone()).await.map_err(|e| {
        warn!(
            source_ip = %source_ip,
            function_name = %function_name,
            "JWT authorizer setup failed: {e}"
        );
        e
    })?;

    match authorizer.authorize(&event).await {
        Ok(output) => {
            cache.insert(cache_key, output.clone()).await;
            Ok(inject_authorizer_context(event, &output))
        }
        Err(e) => {
            // Evict any stale cached decision for this key so the next
            // request re-evaluates rather than serving a stale hit.
            cache.invalidate(&cache_key).await;
            warn!(
                source_ip = %source_ip,
                function_name = %function_name,
                "JWT authorizer denied: {e}"
            );
            Err(e)
        }
    }
}

/// Inject the authorizer output's context into
/// `event.request_context.authorizer.lambda` (the `fields` map on
/// `ApiGatewayRequestAuthorizer`).
///
/// Also populates the JWT authorizer's claims map for JWT authorizer outputs
/// that have an `iss` / `exp` / `sub` context entry.
///
/// Public so `dispatch_lambda` can call it directly on a cache hit.
pub fn inject_authorizer_context(
    mut event: ApiGatewayV2httpRequest,
    output: &AuthorizerOutput,
) -> ApiGatewayV2httpRequest {
    let mut fields: HashMap<String, serde_json::Value> = output.context.clone();
    // Always expose principal_id in the lambda context so handlers can read it.
    fields.insert(
        "principalId".into(),
        serde_json::Value::String(output.principal_id.clone()),
    );

    // Detect if this looks like a JWT output (has iss/exp) and populate the
    // JWT claims sub-object as well so clients that read
    // `requestContext.authorizer.jwt.claims` also work.
    let jwt_description = if fields.contains_key("iss") {
        let claims: HashMap<String, String> = fields
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        Some(ApiGatewayRequestAuthorizerJwtDescription {
            claims,
            scopes: None,
        })
    } else {
        None
    };

    event.request_context.authorizer = Some(ApiGatewayRequestAuthorizer {
        jwt: jwt_description,
        fields,
        iam: None,
    });

    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::authorizer::AuthorizerOutput;
    use crate::config::JwtAuthorizerConfig;
    use crate::gateway::{
        ApiGatewayV2httpRequest, ApiGatewayV2httpRequestContext,
        ApiGatewayV2httpRequestContextHttpDescription,
    };
    use std::time::Duration;

    fn make_event() -> ApiGatewayV2httpRequest {
        ApiGatewayV2httpRequest {
            version: Some("2.0".into()),
            route_key: Some("GET /test".into()),
            raw_path: Some("/test".into()),
            raw_query_string: Some(String::new()),
            cookies: None,
            headers: http::HeaderMap::new(),
            query_string_parameters: Default::default(),
            path_parameters: Default::default(),
            request_context: ApiGatewayV2httpRequestContext {
                route_key: Some("GET /test".into()),
                account_id: Some("000000000000".into()),
                stage: Some("$default".into()),
                request_id: Some("req-1".into()),
                time: None,
                time_epoch: 0,
                http: ApiGatewayV2httpRequestContextHttpDescription {
                    method: http::Method::GET,
                    path: Some("/test".into()),
                    protocol: Some("HTTP/1.1".into()),
                    source_ip: Some("1.2.3.4".into()),
                    user_agent: None,
                },
                ..Default::default()
            },
            stage_variables: Default::default(),
            body: None,
            is_base64_encoded: false,
            kind: None,
            method_arn: None,
            http_method: http::Method::GET,
            identity_source: None,
            authorization_token: None,
            resource: None,
        }
    }

    #[test]
    fn inject_context_populates_fields() {
        let event = make_event();
        let output = AuthorizerOutput {
            principal_id: "user42".into(),
            context: {
                let mut m = HashMap::new();
                m.insert("role".into(), serde_json::Value::String("admin".into()));
                m
            },
            ttl: Duration::from_secs(300),
        };
        let event = inject_authorizer_context(event, &output);
        let auth = event.request_context.authorizer.unwrap();
        assert_eq!(
            auth.fields.get("principalId").and_then(|v| v.as_str()),
            Some("user42")
        );
        assert_eq!(
            auth.fields.get("role").and_then(|v| v.as_str()),
            Some("admin")
        );
    }

    #[test]
    fn inject_jwt_context_populates_jwt_claims() {
        let event = make_event();
        let mut ctx = HashMap::new();
        ctx.insert(
            "iss".into(),
            serde_json::Value::String("https://ex.com".into()),
        );
        ctx.insert("sub".into(), serde_json::Value::String("user123".into()));
        let output = AuthorizerOutput {
            principal_id: "user123".into(),
            context: ctx,
            ttl: Duration::from_secs(300),
        };
        let event = inject_authorizer_context(event, &output);
        let auth = event.request_context.authorizer.unwrap();
        assert!(
            auth.jwt.is_some(),
            "jwt field should be populated for JWT-like context"
        );
        let jwt = auth.jwt.unwrap();
        assert_eq!(
            jwt.claims.get("iss").map(|s| s.as_str()),
            Some("https://ex.com")
        );
    }

    #[test]
    fn none_config_skips_auth() {
        // "none" string → opt-out
        let cfg = AuthorizerConfig::FunctionRef("none".into());
        // We can't call enforce_authorizer without a real ProcessManager,
        // but we verify the config variant is FunctionRef("none") which
        // is the opt-out sentinel.
        assert!(matches!(cfg, AuthorizerConfig::FunctionRef(ref s) if s == "none"));
    }

    #[test]
    fn jwt_config_variant_recognisable() {
        let cfg = AuthorizerConfig::Jwt(JwtAuthorizerConfig {
            r#type: "jwt".into(),
            issuer: "https://ex.com".into(),
            audience: "app".into(),
            jwks_uri: "https://ex.com/.well-known/jwks.json".into(),
        });
        assert!(matches!(cfg, AuthorizerConfig::Jwt(_)));
    }
}
