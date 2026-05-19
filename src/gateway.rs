use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequest {
    pub version: String,
    pub route_key: String,
    pub raw_path: String,
    pub raw_query_string: String,
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub is_base64_encoded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub http: HttpContext,
    pub request_id: String,
    pub time_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpContext {
    pub method: String,
    pub path: String,
    pub protocol: String,
    pub source_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResponse {
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_base64_encoded: Option<bool>,
}

impl GatewayResponse {
    pub fn error(status_code: u16, message: &str) -> Self {
        let body = serde_json::json!({ "error": message }).to_string();
        Self {
            status_code,
            headers: Some(HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )])),
            body: Some(body),
            is_base64_encoded: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_round_trips() {
        let resp = GatewayResponse {
            status_code: 200,
            headers: Some(HashMap::from([("content-type".into(), "text/plain".into())])),
            body: Some("hello".into()),
            is_base64_encoded: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: GatewayResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status_code, 200);
        assert_eq!(back.body.as_deref(), Some("hello"));
    }

    #[test]
    fn request_round_trips() {
        let req = GatewayRequest {
            version: "2.0".into(),
            route_key: "GET /ping".into(),
            raw_path: "/ping".into(),
            raw_query_string: "".into(),
            headers: HashMap::new(),
            request_context: RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/ping".into(),
                    protocol: "HTTP/1.1".into(),
                    source_ip: "127.0.0.1".into(),
                },
                request_id: "abc".into(),
                time_epoch: 1000,
            },
            body: None,
            is_base64_encoded: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.route_key, "GET /ping");
    }

    #[test]
    fn error_response_is_json() {
        let resp = GatewayResponse::error(502, "lambda crashed");
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["error"], "lambda crashed");
        assert_eq!(resp.status_code, 502);
    }

    #[test]
    fn body_omitted_when_none() {
        let resp = GatewayResponse {
            status_code: 204,
            headers: None,
            body: None,
            is_base64_encoded: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("body"));
        assert!(!json.contains("headers"));
        assert!(!json.contains("isBase64Encoded"));
    }
}
