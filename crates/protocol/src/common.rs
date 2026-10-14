use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Extras = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentSource {
    #[serde(rename = "type", default)]
    pub source_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

/// `data:<media>[;base64],<payload>` into an Anthropic content source.
pub fn source_from_data_url(url: &str) -> ContentSource {
    let rest = url.strip_prefix("data:").unwrap_or(url);
    let mut segments = rest.splitn(2, ',');
    let metadata = segments.next().unwrap_or_default();
    let payload = segments.next().unwrap_or_default();
    let encoded = metadata
        .split(';')
        .any(|flag| flag.eq_ignore_ascii_case("base64"));
    let media_type = metadata
        .split(';')
        .next()
        .unwrap_or("application/octet-stream");
    if encoded {
        ContentSource {
            source_type: "base64".into(),
            media_type: Some(if media_type.is_empty() {
                "application/octet-stream".to_string()
            } else {
                media_type.to_string()
            }),
            data: Some(payload.to_string()),
            url: None,
            file_id: None,
            extras: Extras::new(),
        }
    } else {
        ContentSource {
            source_type: "url".into(),
            media_type: None,
            data: None,
            url: Some(url.to_string()),
            file_id: None,
            extras: Extras::new(),
        }
    }
}

pub fn source_to_url(source: &ContentSource) -> Option<String> {
    match source.source_type.as_str() {
        "url" => source.url.clone(),
        "text" => source
            .data
            .as_ref()
            .map(|data| format!("data:text/plain;base64,{data}")),
        _ => source.data.as_ref().map(|data| {
            format!(
                "data:{};base64,{data}",
                source
                    .media_type
                    .as_deref()
                    .unwrap_or("application/octet-stream")
            )
        }),
    }
}

/// The human-readable part of an error payload, whether it is a bare string or
/// an object with a `message` field.
pub fn json_message(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(_) => value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(&value.to_string())
            .to_string(),
        other => other.to_string(),
    }
}

pub fn dedup_notices(notices: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    notices.retain(|notice| seen.insert(notice.clone()));
}

/// Unmodelled inbound keys are only forwarded when they cannot collide with a
/// typed field of the outbound body, which serde would otherwise emit twice.
pub fn merge_extras(target: &mut Extras, source: &Extras, blocked: &[&str]) {
    for (key, value) in source {
        if blocked.contains(&key.as_str()) {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}
