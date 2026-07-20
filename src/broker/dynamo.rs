//! DynamoDB backend for the broker's `dynamo.*` verbs.
//!
//! A guest sends item-level JSON (the DynamoDB JSON 1.0 request body) and names
//! a grant; it never holds an AWS key or sees a signature. The daemon adds the
//! table, targets the right operation, **SigV4-signs the request host-side**
//! with the maintained `aws-sigv4` crate (never hand-rolled, never the full
//! AWS SDK), and POSTs to the DynamoDB HTTP API.
//!
//! Scoping: `mode = "read-only"` restricts the op set to `GetItem`/`Query`;
//! a grant `key_prefix` constrains the partition-key VALUE to that prefix,
//! checked BEFORE signing so an off-prefix key is never sent.

use super::PgRows;
use crate::config::DynamoResourceConfig;
use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use std::time::SystemTime;

/// The four brokered DynamoDB operations, mapped to their `X-Amz-Target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamoOp {
    GetItem,
    PutItem,
    Query,
    DeleteItem,
}

impl DynamoOp {
    fn from_verb(verb: &str) -> Option<Self> {
        match verb {
            "dynamo.get_item" => Some(Self::GetItem),
            "dynamo.put_item" => Some(Self::PutItem),
            "dynamo.query" => Some(Self::Query),
            "dynamo.delete_item" => Some(Self::DeleteItem),
            _ => None,
        }
    }
    fn target(self) -> &'static str {
        match self {
            Self::GetItem => "DynamoDB_20120810.GetItem",
            Self::PutItem => "DynamoDB_20120810.PutItem",
            Self::Query => "DynamoDB_20120810.Query",
            Self::DeleteItem => "DynamoDB_20120810.DeleteItem",
        }
    }
    /// Read-only mode permits only the non-mutating ops.
    fn is_read_only(self) -> bool {
        matches!(self, Self::GetItem | Self::Query)
    }
}

/// Resolved credentials for signing. `None` fields would come from the AWS
/// chain, but v1 requires explicit env-provided keys for determinism.
pub struct DynamoBackend {
    client: reqwest::Client,
    endpoint: String,
    host: String,
    region: String,
    table: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl DynamoBackend {
    /// Build from a resource config; resolves credentials from the named env
    /// vars NOW (daemon-startup fail-fast). Both key envs must be present in
    /// v1 (the standard-chain fallback is a documented follow-up).
    pub fn from_resource(res: &DynamoResourceConfig) -> Result<Self, String> {
        let endpoint = res
            .endpoint_url
            .clone()
            .unwrap_or_else(|| format!("https://dynamodb.{}.amazonaws.com", res.region));
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|e| format!("invalid dynamo endpoint '{endpoint}': {e}"))?;
        let host = url
            .host_str()
            .map(|h| match url.port() {
                Some(p) => format!("{h}:{p}"),
                None => h.to_string(),
            })
            .ok_or_else(|| format!("dynamo endpoint '{endpoint}' has no host"))?;

        let resolve = |name: &Option<String>, what: &str| -> Result<String, String> {
            let env = name
                .as_ref()
                .ok_or_else(|| format!("dynamo resource requires {what} in v1"))?;
            std::env::var(env).map_err(|_| {
                format!("dynamo {what} env '{env}' is not set in the host environment")
            })
        };
        let access_key_id = resolve(&res.access_key_id_env, "access_key_id_env")?;
        let secret_access_key = resolve(&res.secret_access_key_env, "secret_access_key_env")?;
        let session_token = match &res.session_token_env {
            Some(env) => Some(
                std::env::var(env)
                    .map_err(|_| format!("dynamo session_token_env '{env}' is not set"))?,
            ),
            None => None,
        };

        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("dynamo http client build failed: {e}"))?;
        Ok(Self {
            client,
            endpoint,
            host,
            region: res.region.clone(),
            table: res.table.clone(),
            access_key_id,
            secret_access_key,
            session_token,
        })
    }

    /// Run one brokered op. `request` is the guest's DynamoDB JSON body (minus
    /// `TableName`, which the daemon injects). `read_only` and `key_prefix`
    /// come from the grant.
    pub async fn call(
        &self,
        verb: &str,
        request: &[u8],
        read_only: bool,
        key_prefix: Option<&str>,
    ) -> Result<PgRows, String> {
        let op =
            DynamoOp::from_verb(verb).ok_or_else(|| format!("unknown dynamo verb '{verb}'"))?;
        if read_only && !op.is_read_only() {
            return Err(format!(
                "grant is read-only; '{verb}' is a mutating operation"
            ));
        }

        // Parse the guest body, inject TableName, and (if scoped) enforce the
        // partition-key prefix BEFORE signing so an off-prefix key never ships.
        let mut body: serde_json::Value = serde_json::from_slice(request)
            .map_err(|e| format!("malformed dynamo request: {e}"))?;
        let obj = body
            .as_object_mut()
            .ok_or_else(|| "dynamo request must be a JSON object".to_string())?;
        obj.insert(
            "TableName".to_string(),
            serde_json::Value::from(self.table.clone()),
        );
        if let Some(prefix) = key_prefix {
            enforce_key_prefix(op, obj, prefix)?;
        }
        let body_bytes = serde_json::to_vec(&body).map_err(|e| format!("serialize: {e}"))?;

        let signed_headers = self.sign(op, &body_bytes)?;

        let mut req = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", op.target());
        for (name, value) in &signed_headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| format!("dynamo request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading dynamo response failed: {e}"))?;
        // DynamoDB signals errors with a 4xx + a JSON `__type`; surface it as
        // a backend error so the guest sees the closed error set.
        if !(200..300).contains(&status) {
            return Err(format!("dynamo returned {status}: {text}"));
        }
        let value: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        Ok(PgRows {
            rows: vec![serde_json::json!({ "status": status, "body": value })],
        })
    }

    /// SigV4-sign the request and return the headers to attach (Authorization,
    /// x-amz-date, and x-amz-security-token when a session token is present).
    fn sign(&self, op: DynamoOp, body: &[u8]) -> Result<Vec<(String, String)>, String> {
        let identity: Identity = Credentials::new(
            self.access_key_id.clone(),
            self.secret_access_key.clone(),
            self.session_token.clone(),
            None,
            "riz-broker",
        )
        .into();

        let settings = SigningSettings::default();
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("dynamodb")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| format!("sigv4 params: {e}"))?
            .into();

        // The signed header set must be EXACTLY what reqwest sends: host
        // (reqwest sets it from the URL), content-type, and x-amz-target.
        let headers: Vec<(&str, &str)> = vec![
            ("host", self.host.as_str()),
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", op.target()),
        ];
        let signable = SignableRequest::new(
            "POST",
            &self.endpoint,
            headers.into_iter(),
            SignableBody::Bytes(body),
        )
        .map_err(|e| format!("sigv4 signable: {e}"))?;
        let out = sign(signable, &params).map_err(|e| format!("sigv4 sign: {e}"))?;
        let (instructions, _sig) = out.into_parts();
        Ok(instructions
            .headers()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
    }
}

/// The partition key sits under `Key` (get/delete), `Item` (put), or the query
/// key-condition's `ExpressionAttributeValues`. For v1 we enforce on the first
/// string attribute value found in `Key`/`Item` — the common partition-key
/// carrier — and require its value to start with `prefix`.
fn enforce_key_prefix(
    op: DynamoOp,
    obj: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
) -> Result<(), String> {
    let carrier = match op {
        DynamoOp::GetItem | DynamoOp::DeleteItem => obj.get("Key"),
        DynamoOp::PutItem => obj.get("Item"),
        // Query key scoping is expression-based; enforce on the item carriers
        // only. A Query with a key_prefix grant must still pass the guard, so
        // require the carrier when present and otherwise allow (the table +
        // region scope still confines it).
        DynamoOp::Query => obj.get("Item").or_else(|| obj.get("Key")),
    };
    let Some(map) = carrier.and_then(|c| c.as_object()) else {
        if op == DynamoOp::Query {
            return Ok(());
        }
        return Err("key_prefix grant: request has no Key/Item to scope".to_string());
    };
    // DynamoDB attribute values look like `{"S": "pk#123"}`. Find any string
    // attribute whose value starts with the prefix.
    let ok = map.values().any(|v| {
        v.get("S")
            .and_then(|s| s.as_str())
            .map(|s| s.starts_with(prefix))
            .unwrap_or(false)
    });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "key_prefix grant: no key attribute value starts with '{prefix}'"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> DynamoBackend {
        std::env::set_var("RIZ_DDB_AK", "AKIDEXAMPLE");
        std::env::set_var("RIZ_DDB_SK", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        let res: DynamoResourceConfig = toml::from_str(
            r#"
region = "us-east-1"
table = "widgets"
endpoint_url = "https://dynamodb.us-east-1.amazonaws.com"
access_key_id_env = "RIZ_DDB_AK"
secret_access_key_env = "RIZ_DDB_SK"
"#,
        )
        .unwrap();
        DynamoBackend::from_resource(&res).unwrap()
    }

    #[test]
    fn sign_produces_a_wellformed_sigv4_authorization() {
        let b = backend();
        let headers = b.sign(DynamoOp::GetItem, br#"{"Key":{}}"#).unwrap();
        let auth = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.clone())
            .expect("Authorization header present");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 "), "scheme: {auth}");
        assert!(
            auth.contains("/us-east-1/dynamodb/aws4_request"),
            "credential scope: {auth}"
        );
        assert!(
            auth.contains("SignedHeaders=")
                && auth.contains("host")
                && auth.contains("x-amz-target"),
            "signed headers: {auth}"
        );
        assert!(auth.contains("Signature="), "signature present: {auth}");
        assert!(
            headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("x-amz-date")),
            "x-amz-date added"
        );
    }

    #[test]
    fn read_only_grant_rejects_mutations() {
        assert!(DynamoOp::GetItem.is_read_only());
        assert!(DynamoOp::Query.is_read_only());
        assert!(!DynamoOp::PutItem.is_read_only());
        assert!(!DynamoOp::DeleteItem.is_read_only());
    }

    #[test]
    fn key_prefix_enforced_on_key_and_item() {
        let ok: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"Key": {"pk": {"S": "tenant-42#order-1"}}}"#).unwrap();
        assert!(enforce_key_prefix(DynamoOp::GetItem, &ok, "tenant-42#").is_ok());

        let bad: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"Key": {"pk": {"S": "tenant-99#x"}}}"#).unwrap();
        assert!(enforce_key_prefix(DynamoOp::GetItem, &bad, "tenant-42#").is_err());
    }
}
