use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use proxy::{Api, ModelRoute, Protocol, Provider};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
struct Record {
    path: String,
    api_key: Option<String>,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct Mock {
    status: StatusCode,
    reply: Value,
    seen: Arc<Mutex<Vec<Record>>>,
}

impl Mock {
    fn ok() -> Self {
        Self {
            status: StatusCode::OK,
            reply: json!({"id": "x", "object": "chat.completion", "choices": []}),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing(status: u16, reply: Value) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("a valid status"),
            reply,
            seen: Default::default(),
        }
    }

    fn last(&self) -> Record {
        self.seen
            .lock()
            .expect("mock lock")
            .last()
            .cloned()
            .expect("the mock was called")
    }

    fn calls(&self) -> usize {
        self.seen.lock().expect("mock lock").len()
    }
}

async fn handler(State(mock): State<Mock>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let headers: HeaderMap = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(String::from)
    };
    mock.seen.lock().expect("mock lock").push(Record {
        path,
        api_key: header("x-api-key"),
        authorization: header("authorization"),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });

    (
        mock.status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Body::from(mock.reply.to_string()),
    )
        .into_response()
}

async fn spawn(mock: Mock) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let addr = listener.local_addr().expect("a bound address");
    let app = Router::new()
        .route("/{*rest}", any(handler))
        .with_state(mock);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn provider_on(addr: SocketAddr, protocol: Protocol, api: Api) -> Provider {
    Provider {
        name: "probe-target".into(),
        protocol,
        api: Some(api),
        base_url: format!("http://{addr}"),
        api_key: "secret".into(),
        enabled: true,
        patterns: Vec::new(),
        models: vec![ModelRoute {
            public: "alias".into(),
            upstream: "real-model".into(),
            provider: None,
        }],
        headers: Default::default(),
    }
}

#[tokio::test]
async fn a_chat_probe_calls_the_chat_endpoint_with_a_bearer_token() {
    let mock = Mock::ok();
    let addr = spawn(mock.clone()).await;
    let client = proxy::server::reqwest_client();

    let probe =
        proxy::upstream::probe(&client, &provider_on(addr, Protocol::Openai, Api::Chat)).await;
    assert!(probe.ok, "{}", probe.message);
    assert_eq!(probe.status, 200);
    assert!(probe.message.contains("real-model"));

    let seen = mock.last();
    assert_eq!(seen.path, "/v1/chat/completions".to_string());
    assert_eq!(seen.authorization.as_deref(), Some("Bearer secret"));
    assert!(seen.api_key.is_none());
    assert_eq!(seen.body["model"].as_str(), Some("real-model"));
    assert_eq!(seen.body["max_tokens"].as_u64(), Some(1));
}

#[tokio::test]
async fn a_probe_of_the_responses_surface_calls_its_own_path() {
    let mock = Mock::ok();
    let addr = spawn(mock.clone()).await;
    let client = proxy::server::reqwest_client();

    let probe = proxy::upstream::probe(
        &client,
        &provider_on(addr, Protocol::Openai, Api::Responses),
    )
    .await;
    assert!(probe.ok, "{}", probe.message);

    let seen = mock.last();
    assert_eq!(seen.path, "/v1/responses".to_string());
    assert_eq!(
        seen.body["max_output_tokens"].as_u64(),
        Some(16),
        "reasoning models reject a one-token budget"
    );
    assert!(seen.body.get("messages").is_none());
}

#[tokio::test]
async fn a_messages_probe_uses_the_anthropic_headers() {
    let mock = Mock::ok();
    let addr = spawn(mock.clone()).await;
    let client = proxy::server::reqwest_client();

    let probe = proxy::upstream::probe(
        &client,
        &provider_on(addr, Protocol::Anthropic, Api::Messages),
    )
    .await;
    assert!(probe.ok, "{}", probe.message);

    let seen = mock.last();
    assert_eq!(seen.path, "/v1/messages".to_string());
    assert_eq!(seen.api_key.as_deref(), Some("secret"));
    assert!(seen.authorization.is_none());
    assert_eq!(seen.body["max_tokens"].as_u64(), Some(1));
}

#[tokio::test]
async fn an_upstream_rejection_is_reported_with_its_own_words() {
    let mock = Mock::failing(429, json!({"error": {"message": "quota exhausted"}}));
    let addr = spawn(mock).await;
    let client = proxy::server::reqwest_client();

    let probe =
        proxy::upstream::probe(&client, &provider_on(addr, Protocol::Openai, Api::Chat)).await;
    assert!(!probe.ok);
    assert_eq!(probe.status, 429);
    assert_eq!(probe.message, "quota exhausted".to_string());
    assert!(probe.detail.unwrap_or_default().contains("quota exhausted"));
}

#[tokio::test]
async fn an_unreachable_host_is_reported_without_panicking() {
    // Port 1 on loopback has nothing listening.
    let mut provider = provider_on(
        "127.0.0.1:1".parse().expect("an address"),
        Protocol::Openai,
        Api::Chat,
    );
    provider.api_key = String::new();
    let client = proxy::server::reqwest_client();

    let probe = proxy::upstream::probe(&client, &provider).await;
    assert!(!probe.ok);
    assert_eq!(probe.status, 0);
    assert!(
        probe.message.contains("could not reach"),
        "{}",
        probe.message
    );
}

#[tokio::test]
async fn a_provider_without_any_model_says_so_instead_of_calling_out() {
    let mock = Mock::ok();
    let addr = spawn(mock.clone()).await;
    let client = proxy::server::reqwest_client();
    let mut provider = provider_on(addr, Protocol::Openai, Api::Chat);
    provider.models.clear();

    let probe = proxy::upstream::probe(&client, &provider).await;
    assert!(!probe.ok);
    assert!(
        probe.message.contains("no model configured"),
        "{}",
        probe.message
    );
    assert_eq!(
        mock.calls(),
        0,
        "nothing should be sent without a model to ask for"
    );
}

#[tokio::test]
async fn a_pattern_supplies_the_probe_model_when_no_alias_is_configured() {
    let mock = Mock::ok();
    let addr = spawn(mock.clone()).await;
    let client = proxy::server::reqwest_client();
    let mut provider = provider_on(addr, Protocol::Openai, Api::Chat);
    provider.models.clear();
    provider.patterns = vec!["gpt-*".into()];

    let probe = proxy::upstream::probe(&client, &provider).await;
    assert!(probe.ok, "{}", probe.message);
    assert_eq!(
        mock.last().body["model"].as_str(),
        Some("gpt-latest"),
        "a pattern is turned into a guessable model name"
    );
}
