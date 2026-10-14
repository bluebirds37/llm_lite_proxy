use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use bytes::Bytes;
use futures::stream::BoxStream;
use reqwest::Client;
use serde_json::Value;

use crate::config::{Api, Provider};
use crate::error::{upstream_error, Kind, ProxyError};
use crate::logs::TraceHandle;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bearer(value: &str) -> Option<&str> {
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
}

/// The credential the client sent, under either protocol's header.
pub fn forwarded_key(headers: &HeaderMap) -> Option<String> {
    if let Some(key) = header_value(headers, "x-api-key") {
        return Some(key);
    }
    header_value(headers, "authorization")
        .map(|value| bearer(&value).unwrap_or(value.as_str()).to_string())
}

/// A provider with no configured key forwards the client's credential, which is
/// how the proxy is used as a pure protocol shim by bring-your-own-key tooling.
pub fn resolve_key(provider: &Provider, headers: &HeaderMap) -> Option<String> {
    let configured = provider.api_key.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    forwarded_key(headers)
}

pub struct Prepared {
    request: reqwest::RequestBuilder,
    pub url: String,
    pub provider_name: String,
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    client: &Client,
    provider: &Provider,
    key: Option<&str>,
    body: Vec<u8>,
    client_headers: &HeaderMap,
    total_timeout: Option<Duration>,
) -> Prepared {
    let url = provider.url();
    let mut request = client
        .post(url.as_str())
        .header("content-type", "application/json")
        .body(body);

    if let Some(key) = key {
        request = match provider.protocol {
            crate::config::Protocol::Anthropic => request.header("x-api-key", key),
            crate::config::Protocol::Openai => {
                request.header("authorization", format!("Bearer {key}"))
            }
        };
    }
    if provider.protocol == crate::config::Protocol::Anthropic {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(beta) = header_value(client_headers, "anthropic-beta") {
            request = request.header("anthropic-beta", beta);
        }
    }
    for (name, value) in &provider.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(timeout) = total_timeout {
        request = request.timeout(timeout);
    }

    Prepared {
        request,
        url,
        provider_name: provider.name.clone(),
    }
}

pub enum Admitted {
    Json(Value),
    Stream(BoxStream<'static, Result<Bytes, reqwest::Error>>),
}

impl Admitted {
    pub fn is_stream(&self) -> bool {
        matches!(self, Self::Stream(_))
    }
}

/// Sends the request and inspects the response head before any bytes reach the
/// client, so an upstream failure is still reported with a real status code.
pub async fn send(prepared: Prepared, trace: &TraceHandle) -> Result<Admitted, ProxyError> {
    let url = prepared.url.clone();
    let response = prepared.request.send().await.map_err(|error| {
        if error.is_timeout() {
            ProxyError::new(
                StatusCode::GATEWAY_TIMEOUT,
                Kind::Timeout,
                format!("upstream timed out: {url}"),
            )
        } else {
            ProxyError::new(
                StatusCode::BAD_GATEWAY,
                Kind::Upstream,
                format!("could not reach {url}: {error}"),
            )
        }
    })?;

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let error = upstream_error(status, body);
        trace.status(
            u16::from(error.status),
            Some(error.message.clone()),
            error.detail.clone(),
        );
        return Err(error);
    }

    if content_type.contains("event-stream") {
        return Ok(Admitted::Stream(Box::pin(response.bytes_stream())));
    }

    let text = response.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .map(Admitted::Json)
        .map_err(|error| {
            ProxyError::internal(format!("upstream returned unreadable JSON: {error}"))
                .with_detail(text)
        })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Probe {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub message: String,
    pub detail: Option<String>,
}

/// The GUI's "test" button: one one-token completion against the provider as it
/// is configured, so a bad key or URL is visible before any client relies on it.
pub async fn probe(client: &Client, provider: &Provider) -> Probe {
    let Some(model) = probe_model(provider) else {
        return Probe {
            ok: false,
            status: 0,
            latency_ms: 0,
            message: "no model configured: add a model alias or an auto-match pattern first"
                .to_string(),
            detail: None,
        };
    };

    // The body has to match the surface, not just the vendor: an OpenAI
    // Responses endpoint rejects the Chat shape outright.
    let body = match provider.api() {
        Api::Chat | Api::Messages => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "stream": false,
            "messages": [{"role": "user", "content": "ping"}]
        }),
        Api::Responses => serde_json::json!({
            "model": model,
            "max_output_tokens": 16,
            "stream": false,
            "input": "ping"
        }),
    };

    let prepared = build(
        client,
        provider,
        Some(provider.api_key.trim()),
        body.to_string().into_bytes(),
        &HeaderMap::new(),
        Some(Duration::from_secs(20)),
    );
    let started = std::time::Instant::now();
    match prepared.request.send().await {
        Err(error) => Probe {
            ok: false,
            status: 0,
            latency_ms: elapsed(started),
            message: format!("could not reach {}: {error}", prepared.url),
            detail: Some(error.to_string()),
        },
        Ok(response) => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let latency_ms = elapsed(started);
            if status.is_success() {
                return Probe {
                    ok: true,
                    status: u16::from(status),
                    latency_ms,
                    message: format!("{model} answered in {latency_ms} ms"),
                    detail: None,
                };
            }
            Probe {
                ok: false,
                status: u16::from(status),
                latency_ms,
                message: extract(&text, status),
                detail: Some(text),
            }
        }
    }
}

fn elapsed(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn extract(body: &str, status: axum::http::StatusCode) -> String {
    crate::error::extract_message(body).unwrap_or_else(|| {
        format!(
            "upstream returned {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("error")
        )
    })
}

fn probe_model(provider: &Provider) -> Option<String> {
    provider
        .models
        .first()
        .map(|route| route.upstream.clone())
        .or_else(|| {
            provider.patterns.iter().find_map(|pattern| {
                let trimmed = pattern.trim_end_matches('*');
                (!trimmed.is_empty() && trimmed != pattern).then(|| format!("{trimmed}latest"))
            })
        })
}
