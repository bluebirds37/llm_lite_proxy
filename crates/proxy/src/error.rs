use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::config::{Api, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    InvalidRequest,
    Auth,
    NotFound,
    RateLimit,
    Overloaded,
    Timeout,
    Upstream,
    Internal,
}

impl Kind {
    /// Both APIs use the same vocabulary for the subset that overlaps.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request_error",
            Self::Auth => "authentication_error",
            Self::NotFound => "not_found_error",
            Self::RateLimit => "rate_limit_error",
            Self::Overloaded => "overloaded_error",
            Self::Timeout => "timeout_error",
            Self::Upstream => "upstream_error",
            Self::Internal => "api_error",
        }
    }

    pub fn from_status(status: StatusCode) -> Self {
        match status.as_u16() {
            401 | 403 => Self::Auth,
            404 => Self::NotFound,
            408 => Self::Timeout,
            429 => Self::RateLimit,
            400..=499 => Self::InvalidRequest,
            529 => Self::Overloaded,
            500..=599 => Self::Upstream,
            _ => Self::Upstream,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyError {
    pub status: StatusCode,
    pub kind: Kind,
    pub message: String,
    /// Raw upstream response body, kept for the log panel only.
    pub detail: Option<String>,
}

impl ProxyError {
    pub fn new(status: StatusCode, kind: Kind, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Kind::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, Kind::NotFound, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, Kind::Auth, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, Kind::Internal, message)
    }

    pub fn from_conversion(error: protocol::ConversionError) -> Self {
        Self::bad_request(error.to_string())
    }

    pub fn from_config(error: crate::config::ConfigError) -> Self {
        Self::bad_request(error.to_string())
    }
}

impl From<protocol::ConversionError> for ProxyError {
    fn from(error: protocol::ConversionError) -> Self {
        Self::from_conversion(error)
    }
}

/// An upstream failure is reported with the upstream's own status and message;
/// nothing is inferred about why it happened.
pub fn upstream_error(status: StatusCode, body: String) -> ProxyError {
    let plain = body.trim();
    let message = extract_message(&body)
        .or_else(|| {
            if plain.is_empty() {
                None
            } else {
                Some(truncate(plain, 500))
            }
        })
        .unwrap_or_else(|| {
            format!(
                "upstream returned {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("error")
            )
        });
    let kind = Kind::from_status(status);
    ProxyError {
        status,
        kind,
        message,
        detail: None,
    }
    .with_detail(body)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

pub fn extract_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let direct = ["error", "message", "msg"]
        .iter()
        .filter_map(|key| value.get(*key))
        .find_map(|found| found.as_str().map(String::from));
    if let Some(direct) = direct {
        return Some(direct);
    }
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

impl ProxyError {
    pub fn to_json(&self, api: Api) -> serde_json::Value {
        match api.family() {
            Protocol::Openai => json!({
                "error": {
                    "message": self.message,
                    "type": self.kind.wire_name(),
                    "param": serde_json::Value::Null,
                    "code": serde_json::Value::Null,
                }
            }),
            Protocol::Anthropic => json!({
                "type": "error",
                "error": {
                    "type": self.kind.wire_name(),
                    "message": self.message,
                }
            }),
        }
    }

    pub fn into_response_for(self, api: Api) -> Response {
        let status = self.status;
        let body = self.to_json(api);
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_bodies_match_each_protocol() {
        let error = ProxyError::bad_request("model is required");
        let openai = error.to_json(Api::Chat);
        assert_eq!(openai["error"]["type"], json!("invalid_request_error"));
        assert_eq!(openai["error"]["message"], json!("model is required"));

        let anthropic = error.to_json(Api::Messages);
        assert_eq!(anthropic["type"], json!("error"));
        assert_eq!(anthropic["error"]["message"], json!("model is required"));
    }

    #[test]
    fn upstream_messages_are_taken_verbatim() {
        let status = StatusCode::TOO_MANY_REQUESTS;
        let error = upstream_error(
            status,
            r#"{"error":{"message":"Please retry later","type":"rate_limit_error"}}"#.into(),
        );
        assert_eq!(error.message, "Please retry later".to_string());
        assert_eq!(error.kind, Kind::RateLimit);
        assert_eq!(
            error.status,
            StatusCode::TOO_MANY_REQUESTS,
            "the upstream status is preserved"
        );

        let plain = upstream_error(StatusCode::INTERNAL_SERVER_ERROR, "boom".into());
        assert_eq!(plain.message, "boom".to_string());
        assert_eq!(plain.kind, Kind::Upstream);

        let empty = upstream_error(StatusCode::BAD_GATEWAY, String::new());
        assert!(empty.message.contains("502"));
        assert!(empty.detail.is_none(), "an empty body adds no detail");
    }

    #[test]
    fn nested_and_flat_error_shapes_are_both_readable() {
        assert_eq!(
            extract_message(r#"{"error":{"message":"flat nested"}}"#).as_deref(),
            Some("flat nested")
        );
        assert_eq!(
            extract_message(r#"{"message":"top level"}"#).as_deref(),
            Some("top level")
        );
        assert_eq!(
            extract_message(r#"{"error":{"message":{"code":1}}}"#),
            None,
            "a non-string message is not stringified into a lie"
        );
        assert_eq!(extract_message("not json at all"), None);
    }
}
