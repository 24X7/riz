//! Helper crate for writing AWS Lambda handlers in Rust that target the
//! riz host. Mirrors AWS's `lambda_runtime` API surface but reads the riz
//! line-JSON envelope from stdin and writes responses to stdout.

use serde::{Deserialize, Serialize};
use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Lambda context — populated from the riz envelope sidecar fields.
#[derive(Clone, Debug)]
pub struct Context {
    pub function_name: String,
    pub invoked_function_arn: String,
    pub aws_request_id: String,
    deadline_ms: i64,
}

impl Context {
    pub fn get_remaining_time_in_millis(&self) -> i64 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        (self.deadline_ms - now_ms).max(0)
    }
}

#[derive(Deserialize)]
struct Envelope<E> {
    event: E,
    #[serde(rename = "__riz_deadline_ms")]
    deadline_ms: Option<i64>,
    #[serde(rename = "__riz_function_name")]
    function_name: Option<String>,
}

/// Run a handler function. The handler signature is:
/// `async fn handler(event: E, ctx: Context) -> Result<R, Box<dyn std::error::Error>>`
/// where E and R are serde Serialize/Deserialize types (typically
/// aws_lambda_events::apigw::ApiGatewayV2httpRequest / ApiGatewayV2httpResponse).
pub fn run<E, R, F, Fut>(handler: F)
where
    E: for<'de> Deserialize<'de> + 'static,
    R: Serialize,
    F: Fn(E, Context) -> Fut,
    Fut: Future<Output = Result<R, Box<dyn std::error::Error + Send + Sync>>>,
{
    let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
    rt.block_on(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();
        let mut stdout = tokio::io::stdout();
        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break; // EOF
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: Result<Envelope<E>, _> = serde_json::from_str(trimmed);
            let (event, deadline_ms, function_name) = match parsed {
                Ok(env) => (
                    env.event,
                    env.deadline_ms.unwrap_or(0),
                    env.function_name.unwrap_or_default(),
                ),
                Err(_) => {
                    // Fallback: maybe the wire format is a bare event (no envelope).
                    match serde_json::from_str::<E>(trimmed) {
                        Ok(e) => (e, 0, String::new()),
                        Err(e) => {
                            let err = serde_json::json!({
                                "statusCode": 400,
                                "body": format!("envelope parse: {e}")
                            });
                            let _ = stdout.write_all((err.to_string() + "\n").as_bytes()).await;
                            let _ = stdout.flush().await;
                            continue;
                        }
                    }
                }
            };

            let arn = std::env::var("AWS_LAMBDA_FUNCTION_ARN").unwrap_or_else(|_| {
                format!("arn:riz:lambda:local:000000000000:function:{function_name}")
            });
            let ctx = Context {
                function_name,
                invoked_function_arn: arn,
                aws_request_id: uuid::Uuid::new_v4().to_string(),
                deadline_ms,
            };

            let resp = handler(event, ctx).await;
            let resp_json = match resp {
                Ok(r) => serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"statusCode":500,"body":"serialize: {e}"}}"#)),
                Err(e) => format!(r#"{{"statusCode":500,"body":"handler: {e}"}}"#),
            };
            let _ = stdout.write_all((resp_json + "\n").as_bytes()).await;
            let _ = stdout.flush().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct FakeEvent {
        msg: String,
    }

    #[test]
    fn envelope_with_sidecar_fields_deserializes() {
        let json =
            r#"{"event":{"msg":"hello"},"__riz_deadline_ms":9999,"__riz_function_name":"my-fn"}"#;
        let env: Envelope<FakeEvent> = serde_json::from_str(json).expect("must parse");
        assert_eq!(env.event.msg, "hello");
        assert_eq!(env.deadline_ms, Some(9999));
        assert_eq!(env.function_name.as_deref(), Some("my-fn"));
    }

    #[test]
    fn envelope_with_missing_sidecar_fields_defaults_to_none() {
        let json = r#"{"event":{"msg":"world"}}"#;
        let env: Envelope<FakeEvent> = serde_json::from_str(json).expect("must parse");
        assert_eq!(env.event.msg, "world");
        assert_eq!(env.deadline_ms, None);
        assert_eq!(env.function_name, None);
    }

    #[test]
    fn context_remaining_time_is_non_negative() {
        // A past deadline should clamp to zero, not go negative.
        let ctx = Context {
            function_name: "test".into(),
            invoked_function_arn: "arn:riz:lambda:local:0:function:test".into(),
            aws_request_id: "req-1".into(),
            deadline_ms: 0, // epoch 0 — always in the past
        };
        assert_eq!(
            ctx.get_remaining_time_in_millis(),
            0,
            "past deadline must clamp to zero"
        );
    }

    #[test]
    fn context_remaining_time_positive_for_future_deadline() {
        let future_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
            + 60_000; // 60 seconds from now
        let ctx = Context {
            function_name: "test".into(),
            invoked_function_arn: "arn:riz:lambda:local:0:function:test".into(),
            aws_request_id: "req-2".into(),
            deadline_ms: future_ms,
        };
        assert!(
            ctx.get_remaining_time_in_millis() > 0,
            "future deadline must yield positive remaining time"
        );
    }
}
