//! S3 backend for the broker's `s3.*` verbs.
//!
//! A guest names a grant and an object key; it never holds an AWS key or sees a
//! signature. The daemon builds the object URL, **SigV4-signs the request
//! host-side** with the maintained `aws-sigv4` crate (never hand-rolled, never
//! the full AWS SDK — that crate ships only in `deploy.rs`), and sends it to the
//! S3 REST API.
//!
//! S3 differs from the `dynamo` backend in exactly the ways the REST protocol
//! demands: verbs map to HTTP methods (GET/PUT/DELETE) with the key in the URL
//! path (not a single POST + `X-Amz-Target`), and the signed header set includes
//! `x-amz-content-sha256` (the payload hash), which the crate emits when
//! `payload_checksum_kind` is set. Everything else — credential resolution,
//! read-only scoping, `key_prefix` scoping checked BEFORE signing, the shared
//! response envelope — mirrors `dynamo`.
//!
//! Body handling in v1: the object body is returned as a UTF-8 string (like the
//! `http` backend). Text/JSON/XML objects round-trip exactly; base64 for binary
//! objects is a documented follow-up.

use super::PgRows;
use crate::config::S3ResourceConfig;
use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    sign, PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use std::time::SystemTime;

/// The four brokered S3 operations, mapped to their HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3Op {
    GetObject,
    PutObject,
    ListObjects,
    DeleteObject,
}

impl S3Op {
    fn from_verb(verb: &str) -> Option<Self> {
        match verb {
            "s3.get_object" => Some(Self::GetObject),
            "s3.put_object" => Some(Self::PutObject),
            "s3.list_objects" => Some(Self::ListObjects),
            "s3.delete_object" => Some(Self::DeleteObject),
            _ => None,
        }
    }
    fn method(self) -> &'static str {
        match self {
            Self::GetObject | Self::ListObjects => "GET",
            Self::PutObject => "PUT",
            Self::DeleteObject => "DELETE",
        }
    }
    /// Read-only mode permits only the non-mutating ops.
    fn is_read_only(self) -> bool {
        matches!(self, Self::GetObject | Self::ListObjects)
    }
}

/// A brokered S3 bucket. Credentials resolve host-side and never cross to the
/// guest; the daemon SigV4-signs each request.
pub struct S3Backend {
    client: reqwest::Client,
    /// Base URL WITHOUT a trailing key: virtual-host `https://<bucket>.s3.<region>.amazonaws.com`
    /// for real AWS, or path-style `<endpoint_url>/<bucket>` when an endpoint is
    /// set (DynamoDB-Local-style mocks, MinIO).
    base: String,
    host: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl S3Backend {
    /// Build from a resource config; resolves credentials from the named env
    /// vars NOW (daemon-startup fail-fast). Both key envs must be present in
    /// v1 (the standard-chain fallback is a documented follow-up).
    pub fn from_resource(res: &S3ResourceConfig) -> Result<Self, String> {
        let base = match &res.endpoint_url {
            // Path-style so a local mock / MinIO with no per-bucket DNS works.
            Some(ep) => format!("{}/{}", ep.trim_end_matches('/'), res.bucket),
            // Virtual-hosted style — the bucket rides the host, as real S3 wants.
            None => format!("https://{}.s3.{}.amazonaws.com", res.bucket, res.region),
        };
        let url =
            reqwest::Url::parse(&base).map_err(|e| format!("invalid s3 endpoint '{base}': {e}"))?;
        let host = url
            .host_str()
            .map(|h| match url.port() {
                Some(p) => format!("{h}:{p}"),
                None => h.to_string(),
            })
            .ok_or_else(|| format!("s3 endpoint '{base}' has no host"))?;

        let resolve = |name: &Option<String>, what: &str| -> Result<String, String> {
            let env = name
                .as_ref()
                .ok_or_else(|| format!("s3 resource requires {what} in v1"))?;
            std::env::var(env)
                .map_err(|_| format!("s3 {what} env '{env}' is not set in the host environment"))
        };
        let access_key_id = resolve(&res.access_key_id_env, "access_key_id_env")?;
        let secret_access_key = resolve(&res.secret_access_key_env, "secret_access_key_env")?;
        let session_token = match &res.session_token_env {
            Some(env) => Some(
                std::env::var(env)
                    .map_err(|_| format!("s3 session_token_env '{env}' is not set"))?,
            ),
            None => None,
        };

        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("s3 http client build failed: {e}"))?;
        Ok(Self {
            client,
            base,
            host,
            region: res.region.clone(),
            access_key_id,
            secret_access_key,
            session_token,
        })
    }

    /// Run one brokered op. `request` is the guest's JSON: `{"key": ...}` for
    /// get/delete, `{"key": ..., "body": ...}` for put, `{"prefix": ...}` for
    /// list. `read_only` and `key_prefix` come from the grant; `key_prefix` is
    /// enforced on the object key BEFORE signing so an off-prefix key never
    /// ships.
    pub async fn call(
        &self,
        verb: &str,
        request: &[u8],
        read_only: bool,
        key_prefix: Option<&str>,
    ) -> Result<PgRows, String> {
        let op = S3Op::from_verb(verb).ok_or_else(|| format!("unknown s3 verb '{verb}'"))?;
        if read_only && !op.is_read_only() {
            return Err(format!(
                "grant is read-only; '{verb}' is a mutating operation"
            ));
        }
        let req: serde_json::Value =
            serde_json::from_slice(request).map_err(|e| format!("malformed s3 request: {e}"))?;

        let (url, body_bytes) = match op {
            S3Op::GetObject | S3Op::DeleteObject | S3Op::PutObject => {
                let key = req
                    .get("key")
                    .and_then(|k| k.as_str())
                    .ok_or_else(|| format!("s3 '{verb}' requires a string 'key'"))?;
                enforce_key_prefix(key, key_prefix)?;
                let body = match op {
                    S3Op::PutObject => req
                        .get("body")
                        .and_then(|b| b.as_str())
                        .unwrap_or("")
                        .as_bytes()
                        .to_vec(),
                    _ => Vec::new(),
                };
                (format!("{}/{}", self.base, encode_key(key)), body)
            }
            S3Op::ListObjects => {
                let prefix = req.get("prefix").and_then(|p| p.as_str()).unwrap_or("");
                // A grant `key_prefix` confines the listing: the effective
                // prefix is the grant prefix followed by the requested one, so
                // a guest can never list outside its scope.
                let effective = match key_prefix {
                    Some(kp) => format!("{kp}{prefix}"),
                    None => prefix.to_string(),
                };
                (
                    format!(
                        "{}/?list-type=2&prefix={}",
                        self.base,
                        encode_query(&effective)
                    ),
                    Vec::new(),
                )
            }
        };

        let signed_headers = self.sign(op.method(), &url, &body_bytes)?;

        let method = reqwest::Method::from_bytes(op.method().as_bytes())
            .map_err(|e| format!("invalid method: {e}"))?;
        let mut http_req = self.client.request(method, &url);
        for (name, value) in &signed_headers {
            http_req = http_req.header(name.as_str(), value.as_str());
        }
        let resp = http_req
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| format!("s3 request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading s3 response failed: {e}"))?;
        // S3 signals errors with a 4xx/5xx + an XML <Error>; surface it as a
        // backend error so the guest sees the closed error set.
        if !(200..300).contains(&status) {
            return Err(format!("s3 returned {status}: {text}"));
        }
        Ok(PgRows {
            rows: vec![serde_json::json!({ "status": status, "body": text })],
        })
    }

    /// SigV4-sign the request and return the headers to attach (Authorization,
    /// x-amz-date, x-amz-content-sha256, and x-amz-security-token when a session
    /// token is present). The `host` header is the only request header signed;
    /// reqwest sets it from the URL, matching what we sign here.
    fn sign(&self, method: &str, url: &str, body: &[u8]) -> Result<Vec<(String, String)>, String> {
        let identity: Identity = Credentials::new(
            self.access_key_id.clone(),
            self.secret_access_key.clone(),
            self.session_token.clone(),
            None,
            "riz-broker",
        )
        .into();

        // S3 requires the payload hash header in the signed set — the crate
        // emits `x-amz-content-sha256` when asked, so we never hand-hash.
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| format!("sigv4 params: {e}"))?
            .into();

        let headers: Vec<(&str, &str)> = vec![("host", self.host.as_str())];
        let signable =
            SignableRequest::new(method, url, headers.into_iter(), SignableBody::Bytes(body))
                .map_err(|e| format!("sigv4 signable: {e}"))?;
        let out = sign(signable, &params).map_err(|e| format!("sigv4 sign: {e}"))?;
        let (instructions, _sig) = out.into_parts();
        Ok(instructions
            .headers()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
    }
}

/// The object key must start with the grant's `key_prefix` (when scoped),
/// checked before signing so an off-prefix key never ships.
fn enforce_key_prefix(key: &str, key_prefix: Option<&str>) -> Result<(), String> {
    match key_prefix {
        Some(prefix) if !key.starts_with(prefix) => Err(format!(
            "key_prefix grant: object key '{key}' is not under '{prefix}'"
        )),
        _ => Ok(()),
    }
}

/// Percent-encode an object key's path segments, preserving `/` separators so a
/// nested key like `a/b/c.json` stays a path. Encodes each segment; keeps the
/// canonical form identical between the signer and reqwest.
fn encode_key(key: &str) -> String {
    key.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode a single path/query token per RFC 3986 unreserved set.
fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for byte in seg.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// Query values encode the same unreserved set (S3's list prefix).
fn encode_query(value: &str) -> String {
    encode_segment(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> S3Backend {
        std::env::set_var("RIZ_S3_AK", "AKIDEXAMPLE");
        std::env::set_var("RIZ_S3_SK", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        let res: S3ResourceConfig = toml::from_str(
            r#"
region = "us-east-1"
bucket = "widgets"
endpoint_url = "https://s3.us-east-1.amazonaws.com"
access_key_id_env = "RIZ_S3_AK"
secret_access_key_env = "RIZ_S3_SK"
"#,
        )
        .unwrap();
        S3Backend::from_resource(&res).unwrap()
    }

    #[test]
    fn sign_produces_a_wellformed_sigv4_authorization_with_payload_hash() {
        let b = backend();
        let headers = b
            .sign(
                "GET",
                "https://s3.us-east-1.amazonaws.com/widgets/report.json",
                b"",
            )
            .unwrap();
        let auth = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.clone())
            .expect("Authorization header present");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 "), "scheme: {auth}");
        assert!(
            auth.contains("/us-east-1/s3/aws4_request"),
            "credential scope names the s3 service: {auth}"
        );
        assert!(
            auth.contains("SignedHeaders=") && auth.contains("x-amz-content-sha256"),
            "payload hash is a signed header: {auth}"
        );
        assert!(auth.contains("Signature="), "signature present: {auth}");
        assert!(
            headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("x-amz-content-sha256")),
            "x-amz-content-sha256 header emitted"
        );
    }

    #[test]
    fn read_only_permits_reads_rejects_writes() {
        assert!(S3Op::GetObject.is_read_only());
        assert!(S3Op::ListObjects.is_read_only());
        assert!(!S3Op::PutObject.is_read_only());
        assert!(!S3Op::DeleteObject.is_read_only());
    }

    #[test]
    fn key_prefix_scopes_the_object_key() {
        assert!(enforce_key_prefix("tenant-42/report.json", Some("tenant-42/")).is_ok());
        assert!(enforce_key_prefix("tenant-99/report.json", Some("tenant-42/")).is_err());
        assert!(enforce_key_prefix("anything", None).is_ok());
    }

    #[test]
    fn nested_keys_encode_but_keep_slashes() {
        assert_eq!(encode_key("a/b/c.json"), "a/b/c.json");
        assert_eq!(encode_key("has space/x"), "has%20space/x");
    }
}
