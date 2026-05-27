//! REQUEST authorizer — calls a user-declared riz function as the authorizer.
//!
//! The authorizer function receives the full `ApiGatewayV2httpRequest` event
//! (same shape as a normal handler invocation) and must return one of:
//!
//! **Simple-response format (preferred for HTTP APIs):**
//! ```json
//! {
//!   "isAuthorized": true,
//!   "context": { "userId": "abc123" }
//! }
//! ```
//!
//! **IAM-policy format:**
//! ```json
//! {
//!   "principalId": "user123",
//!   "policyDocument": { "Statement": [{ "Effect": "Allow", "Action": "execute-api:Invoke" }] },
//!   "context": { "userId": "abc123" }
//! }
//! ```
//!
//! For IAM-policy format, `Effect != "Allow"` returns 403 Forbidden.
//! For simple-response format, `isAuthorized: false` returns 401 Unauthorized.
//!
//! Reference: <https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-lambda-authorizer.html>

use crate::auth::authorizer::{AuthError, Authorizer, AuthorizerOutput};
use crate::gateway::ApiGatewayV2httpRequest;
use crate::process::ProcessManager;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Authorizer simple-response payload expected from the authorizer function.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizerSimpleResponse {
    is_authorized: bool,
    #[serde(default)]
    context: HashMap<String, Value>,
}

/// IAM policy document statement.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PolicyStatement {
    effect: String,
    #[allow(dead_code)]
    action: Option<serde_json::Value>,
    #[allow(dead_code)]
    resource: Option<serde_json::Value>,
}

/// IAM-policy format response from the authorizer function.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizerIamResponse {
    #[serde(default)]
    principal_id: Option<String>,
    policy_document: Option<IamPolicyDocument>,
    #[serde(default)]
    context: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IamPolicyDocument {
    statement: Vec<PolicyStatement>,
}

/// Combined response: try simple-response first, fall back to IAM-policy.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AuthorizerResponse {
    Simple(AuthorizerSimpleResponse),
    Iam(AuthorizerIamResponse),
}

impl Default for AuthorizerResponse {
    fn default() -> Self {
        Self::Simple(AuthorizerSimpleResponse::default())
    }
}

/// REQUEST authorizer: invokes a named riz function as the authorization gate.
///
/// The authorizer function receives the same `ApiGatewayV2httpRequest` the
/// user sent. It must reply with `{"isAuthorized": bool, "context": {...}}`.
pub struct RequestAuthorizer {
    /// Name of the riz function to invoke as authorizer (must exist in the pool).
    authorizer_fn: String,
    /// Timeout for authorizer invocations — shorter than handler timeout to
    /// ensure auth failures respond fast.
    timeout_ms: u64,
    process_manager: Arc<ProcessManager>,
}

impl RequestAuthorizer {
    pub fn new(authorizer_fn: impl Into<String>, process_manager: Arc<ProcessManager>) -> Self {
        Self {
            authorizer_fn: authorizer_fn.into(),
            timeout_ms: 5_000,
            process_manager,
        }
    }

    /// Override the default 5s authorizer timeout.
    ///
    /// Useful in tests and for operators who want tighter SLAs on auth
    /// (e.g. `timeout_ms = 1000` to fail fast if the authorizer is slow).
    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }
}

#[async_trait::async_trait]
impl Authorizer for RequestAuthorizer {
    async fn authorize(
        &self,
        event: &ApiGatewayV2httpRequest,
    ) -> Result<AuthorizerOutput, AuthError> {
        let source_ip = event
            .request_context
            .http
            .source_ip
            .as_deref()
            .unwrap_or("unknown");

        let response: AuthorizerResponse = self
            .process_manager
            .invoke_generic(&self.authorizer_fn, event, self.timeout_ms)
            .await
            .map_err(|e| {
                warn!(
                    authorizer_fn = %self.authorizer_fn,
                    source_ip = %source_ip,
                    "REQUEST authorizer invocation failed: {e}"
                );
                AuthError::Other(format!("authorizer invoke error: {e}"))
            })?;

        match response {
            AuthorizerResponse::Simple(simple) => {
                if !simple.is_authorized {
                    warn!(
                        authorizer_fn = %self.authorizer_fn,
                        source_ip = %source_ip,
                        "REQUEST authorizer denied request (simple response)"
                    );
                    return Err(AuthError::Unauthorized(format!(
                        "request denied by authorizer '{}'",
                        self.authorizer_fn
                    )));
                }

                let mut context = simple.context;
                let principal_id = context
                    .remove("principalId")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "authorized".to_string());

                Ok(AuthorizerOutput {
                    principal_id,
                    context,
                    ttl: Duration::from_secs(300),
                })
            }
            AuthorizerResponse::Iam(iam) => {
                // Check the first Allow statement. If any statement has Effect = "Allow",
                // the request is permitted. Otherwise, return 403 Forbidden.
                let allowed = iam
                    .policy_document
                    .as_ref()
                    .map(|pd| {
                        pd.statement
                            .iter()
                            .any(|s| s.effect.eq_ignore_ascii_case("Allow"))
                    })
                    .unwrap_or(false);

                if !allowed {
                    warn!(
                        authorizer_fn = %self.authorizer_fn,
                        source_ip = %source_ip,
                        "REQUEST authorizer returned IAM policy with Effect != Allow"
                    );
                    return Err(AuthError::Forbidden(format!(
                        "IAM policy from authorizer '{}' denies access",
                        self.authorizer_fn
                    )));
                }

                let mut context = iam.context;
                let principal_id = iam
                    .principal_id
                    .or_else(|| {
                        context
                            .remove("principalId")
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                    })
                    .unwrap_or_else(|| "authorized".to_string());

                Ok(AuthorizerOutput {
                    principal_id,
                    context,
                    ttl: Duration::from_secs(300),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorizer_simple_response_deserializes_authorized() {
        let json = r#"{"isAuthorized": true, "context": {"userId": "abc"}}"#;
        let r: AuthorizerSimpleResponse = serde_json::from_str(json).unwrap();
        assert!(r.is_authorized);
        assert_eq!(
            r.context.get("userId").and_then(|v| v.as_str()),
            Some("abc")
        );
    }

    #[test]
    fn authorizer_simple_response_deserializes_denied() {
        let json = r#"{"isAuthorized": false}"#;
        let r: AuthorizerSimpleResponse = serde_json::from_str(json).unwrap();
        assert!(!r.is_authorized);
        assert!(r.context.is_empty());
    }

    #[test]
    fn authorizer_simple_response_rejects_missing_is_authorized() {
        let json = r#"{"context": {"x": 1}}"#;
        // isAuthorized is required (no default) — must fail
        let r: Result<AuthorizerSimpleResponse, _> = serde_json::from_str(json);
        assert!(r.is_err(), "missing isAuthorized must be a parse error");
    }

    #[test]
    fn authorizer_simple_response_default_is_denied() {
        let d = AuthorizerSimpleResponse::default();
        assert!(!d.is_authorized);
    }
}
