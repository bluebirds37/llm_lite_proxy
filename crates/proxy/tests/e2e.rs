use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use futures::stream::StreamExt;
use proxy::{Config, ModelRoute, Protocol, Provider, Proxy};
use serde_json::{json, Value};

#[derive(Clone)]
struct Mock {
    status: u16,
    json: Value,
    sse: Vec<String>,
    split_frames: bool,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Mock {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            json: body,
            sse: Vec::new(),
            split_frames: false,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn streaming(frames: Vec<String>) -> Self {
        Self {
            status: 200,
            json: Value::Null,
            sse: frames,
            split_frames: false,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn last_request(&self) -> Value {
        self.seen
            .lock()
            .expect("mock lock")
            .last()
            .cloned()
            .expect("the mock saw a request")
    }

    fn requests(&self) -> usize {
        self.seen.lock().expect("mock lock").len()
    }
}

async fn mock_handler(State(mock): State<Mock>, request: Request) -> Response {
    let body = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap_or_default();
    if let Ok(value) = serde_json::from_slice::<Value>(&body) {
        mock.seen.lock().expect("mock lock").push(value);
    }

    if mock.status != 200 {
        return (
            StatusCode::from_u16(mock.status).unwrap_or(StatusCode::BAD_GATEWAY),
            [(header::CONTENT_TYPE, "application/json")],
            mock.json.to_string(),
        )
            .into_response();
    }

    if mock.sse.is_empty() {
        return (
            [(header::CONTENT_TYPE, "application/json")],
            mock.json.to_string(),
        )
            .into_response();
    }

    let mut chunks: Vec<Bytes> = Vec::new();
    for frame in mock.sse.clone() {
        if mock.split_frames {
            let middle = frame.len() / 2;
            chunks.push(Bytes::from(frame[..middle].to_string()));
            chunks.push(Bytes::from(frame[middle..].to_string()));
        } else {
            chunks.push(Bytes::from(frame));
        }
    }
    let stream = futures::stream::iter(chunks.into_iter().map(Ok::<Bytes, std::io::Error>));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .expect("a simple SSE response builds")
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let addr = listener.local_addr().expect("a bound address");
    let app = Router::new()
        .route("/{*rest}", any(mock_handler))
        .with_state(mock);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn config_for(addr: SocketAddr, protocol: Protocol) -> Config {
    Config {
        server: proxy::Server {
            host: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        },
        providers: vec![Provider {
            name: "mock".into(),
            protocol,
            api: None,
            base_url: format!("http://{addr}"),
            api_key: "upstream-secret".into(),
            enabled: true,
            patterns: vec!["claude*".into(), "gpt*".into()],
            models: vec![ModelRoute {
                public: "alias".into(),
                upstream: "real-model".into(),
                provider: None,
            }],
            headers: Default::default(),
        }],
    }
}

async fn proxy_for(protocol: Protocol) -> (Proxy, Mock) {
    let mock = match protocol {
        Protocol::Openai => Mock::json(
            200,
            json!({
                "id": "chatcmpl-9", "object": "chat.completion", "created": 1, "model": "real-model",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "from openai"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}
            }),
        ),
        Protocol::Anthropic => Mock::json(
            200,
            json!({
                "id": "msg-9", "type": "message", "role": "assistant", "model": "real-model",
                "content": [{"type": "text", "text": "from anthropic"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 8, "output_tokens": 3}
            }),
        ),
    };
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, protocol))
        .await
        .expect("the proxy binds");
    (proxy, mock)
}

fn url(proxy: &Proxy, path: &str) -> String {
    format!("http://{}{path}", proxy.addr())
}

async fn post_json(
    proxy: &Proxy,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, String) {
    let mut request = reqwest::Client::new().post(url(proxy, path)).json(&body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("the proxy answers");
    let status = response.status();
    let text = response.text().await.expect("a body");
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.clone()));
    (status, value, text)
}

async fn post_stream(proxy: &Proxy, path: &str, body: Value) -> String {
    let response = reqwest::Client::new()
        .post(url(proxy, path))
        .json(&body)
        .send()
        .await
        .expect("the proxy answers");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .contains("text/event-stream"));

    let mut text = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        text.push_str(&String::from_utf8_lossy(&chunk.expect("a streamed chunk")));
    }
    text
}

fn frames_to_text(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter(|frame| !frame.trim().is_empty())
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
        })
        .collect()
}

const ANTHROPIC_REPLY_FRAMES: &[&str] = &[
    r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"real-model","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}

"#,
    r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}

"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}

"#,
    r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}

"#,
    r#"event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_time","input":{}}}

"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\""}}

"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":": \"Oslo\"}"}}

"#,
    r#"event: content_block_stop
data: {"type":"content_block_stop","index":1}

"#,
    r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}

"#,
    r#"event: message_stop
data: {"type":"message_stop"}

"#,
];

const OPENAI_REPLY_FRAMES: &[&str] = &[
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{"content":"Hel"}}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{"content":"lo"}}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_time","arguments":"{\"a\""}}]}}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

"#,
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"real-model","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":4,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":3}}}

"#,
    r#"data: [DONE]

"#,
];

#[tokio::test]
async fn openai_client_against_an_anthropic_upstream() {
    let addr = {
        let mock = Mock::json(
            200,
            json!({
                "id": "msg_7", "type": "message", "role": "assistant", "model": "real-model",
                "content": [
                    {"type": "text", "text": "looking"},
                    {"type": "tool_use", "id": "toolu_7", "name": "get_time", "input": {"city": "Oslo"}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 5, "output_tokens": 9, "cache_read_input_tokens": 4}
            }),
        );
        spawn_mock(mock.clone()).await
    };
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let (status, body, _) = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({
            "model": "alias",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "time in Oslo?"}
            ],
            "tools": [{"type": "function", "function": {"name": "get.time", "parameters": {"type": "object"}}}],
            "presence_penalty": 0.5
        }),
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        proxy.state.config.load().providers[0].url() == format!("http://{addr}/v1/messages"),
        "the anthropic endpoint is built from a bare host base_url"
    );

    assert_eq!(
        body["choices"][0]["message"]["content"].as_str(),
        Some("looking")
    );
    let call = &body["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"].as_str(), Some("get.time"));
    assert_eq!(call["id"].as_str(), Some("toolu_7"));
    assert_eq!(
        call["function"]["arguments"].as_str(),
        Some(r#"{"city":"Oslo"}"#)
    );
    assert_eq!(
        body["choices"][0]["finish_reason"].as_str(),
        Some("tool_calls")
    );
    assert_eq!(body["id"].as_str(), Some("msg_7"));
    assert_eq!(body["usage"]["prompt_tokens"].as_u64(), Some(9));
    assert_eq!(body["usage"]["completion_tokens"].as_u64(), Some(9));
}

#[tokio::test]
async fn the_anthropic_upstream_request_is_well_formed() {
    let mock = Mock::json(
        200,
        json!({"id":"m","type":"message","role":"assistant","model":"real-model",
               "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn",
               "usage":{"input_tokens":1,"output_tokens":1}}),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    post_json(
        &proxy,
        "/v1/chat/completions",
        json!({
            "model": "alias",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two", "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get.time", "arguments": "{\"x\":1}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "three"}
            ],
            "stop": ["END"],
            "max_tokens": 250
        }),
        &[],
    )
    .await;

    let seen = mock.last_request();
    assert_eq!(seen["model"].as_str(), Some("real-model"));
    assert_eq!(seen["system"][0]["text"].as_str(), Some("be brief"));
    assert_eq!(seen["messages"].as_array().unwrap().len(), 3);
    assert_eq!(seen["messages"][1]["role"].as_str(), Some("assistant"));
    assert_eq!(
        seen["messages"][1]["content"][1]["type"].as_str(),
        Some("tool_use")
    );
    assert_eq!(
        seen["messages"][2]["content"][0]["type"].as_str(),
        Some("tool_result")
    );
    assert_eq!(seen["max_tokens"].as_u64(), Some(250));
    assert_eq!(seen["stop_sequences"][0].as_str(), Some("END"));
    assert!(seen.get("presence_penalty").is_none());
    assert_eq!(seen["messages"][0]["role"].as_str(), Some("user"));
}

#[tokio::test]
async fn openai_client_streaming_from_an_anthropic_upstream() {
    let mock = Mock::streaming(
        ANTHROPIC_REPLY_FRAMES
            .iter()
            .map(|frame| frame.to_string())
            .collect(),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let text = post_stream(
        &proxy,
        "/v1/chat/completions",
        json!({
            "model": "alias", "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "get.time", "parameters": {"type": "object"}}}]
        }),
    )
    .await;
    let chunks = frames_to_text(&text);

    let content: String = chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(content, "Hello".to_string());

    let arguments: String = chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|call| {
            call.pointer("/function/arguments")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    assert_eq!(arguments, r#"{"city": "Oslo"}"#.to_string());

    let named = chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/tool_calls/0/function/name")
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(named, vec!["get.time"], "the sanitised name is undone");

    assert!(chunks.iter().any(|chunk| chunk
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("tool_calls")));
    let usage = chunks
        .iter()
        .find(|chunk| chunk.get("usage").is_some())
        .expect("a usage frame");
    assert_eq!(usage["usage"]["prompt_tokens"].as_u64(), Some(12));
    assert_eq!(usage["usage"]["completion_tokens"].as_u64(), Some(7));
    assert!(text.ends_with("data: [DONE]\n\n"));
    assert_eq!(chunks[0]["object"].as_str(), Some("chat.completion.chunk"));
}

#[tokio::test]
async fn anthropic_client_streaming_from_an_openai_upstream() {
    let mock = Mock::streaming(
        OPENAI_REPLY_FRAMES
            .iter()
            .map(|frame| frame.to_string())
            .collect(),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Openai))
        .await
        .unwrap();

    let text = post_stream(
        &proxy,
        "/v1/messages",
        json!({"model": "alias", "max_tokens": 100, "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;

    let names: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("event: "))
        .collect();
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert!(!text.contains("[DONE]"));

    let payloads = frames_to_text(&text);
    let start = &payloads[0]["message"];
    assert_eq!(start["id"].as_str(), Some("chatcmpl-1"));
    assert_eq!(start["model"].as_str(), Some("real-model"));
    let tool_start = &payloads[5]["content_block"];
    assert_eq!(tool_start["type"].as_str(), Some("tool_use"));
    assert_eq!(tool_start["name"].as_str(), Some("get_time"));
    let delta = &payloads[payloads.len() - 2];
    assert_eq!(delta["delta"]["stop_reason"].as_str(), Some("tool_use"));
    assert_eq!(delta["usage"]["input_tokens"].as_u64(), Some(6));
    assert_eq!(delta["usage"]["output_tokens"].as_u64(), Some(4));
    assert_eq!(delta["usage"]["cache_read_input_tokens"].as_u64(), Some(3));

    let logged = &proxy.state.logs.recent(5)[0];
    assert_eq!(
        (logged.input_tokens, logged.output_tokens),
        (9, 4),
        "a chat upstream feeds the log tokens even when the stream is converted"
    );
}

#[tokio::test]
async fn a_stream_broken_across_tcp_boundaries_is_still_reassembled() {
    let mut mock = Mock::streaming(
        ANTHROPIC_REPLY_FRAMES
            .iter()
            .map(|frame| frame.to_string())
            .collect(),
    );
    mock.split_frames = true;
    let addr = spawn_mock(mock).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let text = post_stream(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "alias", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;

    let content: String = frames_to_text(&text)
        .iter()
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(content, "Hello".to_string());
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn anthropic_client_against_an_openai_upstream_converts_the_reply() {
    let mock = Mock::json(
        200,
        json!({
            "id": "chatcmpl-3", "object": "chat.completion", "created": 3, "model": "real-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "plain",
                "tool_calls": [{"id": "call_3", "type": "function", "function": {"name": "get_time", "arguments": "{\"z\":1}"}}]},
                "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12,
                       "prompt_tokens_details": {"cached_tokens": 4}}
        }),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Openai))
        .await
        .unwrap();

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/messages",
        json!({
            "model": "alias",
            "max_tokens": 128,
            "system": "be brief",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_3", "name": "get.time", "input": {"z": 1}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_3", "content": "22:00"}]}
            ],
            "tools": [{"name": "get.time", "input_schema": {"type": "object"}}],
            "stop_sequences": ["STOP"],
            "metadata": {"user_id": "u-9"}
        }),
        &[],
    )
    .await;

    assert_eq!(body["type"].as_str(), Some("message"));
    assert_eq!(body["id"].as_str(), Some("chatcmpl-3"));
    assert_eq!(body["stop_reason"].as_str(), Some("tool_use"));
    assert_eq!(body["content"][0]["type"].as_str(), Some("text"));
    assert_eq!(body["content"][0]["text"].as_str(), Some("plain"));
    assert_eq!(body["content"][1]["name"].as_str(), Some("get.time"));
    assert_eq!(body["usage"]["input_tokens"].as_u64(), Some(6));
    assert_eq!(body["usage"]["cache_read_input_tokens"].as_u64(), Some(4));
    assert_eq!(body["usage"]["output_tokens"].as_u64(), Some(2));

    let seen = mock.last_request();
    assert_eq!(seen["model"].as_str(), Some("real-model"));
    assert_eq!(seen["messages"][0]["role"].as_str(), Some("system"));
    assert_eq!(seen["messages"][2]["role"].as_str(), Some("assistant"));
    assert_eq!(
        seen["messages"][2]["tool_calls"][0]["function"]["name"].as_str(),
        Some("get_time")
    );
    assert_eq!(seen["messages"][3]["role"].as_str(), Some("tool"));
    assert_eq!(
        seen["messages"][3]["tool_call_id"].as_str(),
        Some("toolu_3")
    );
    assert_eq!(
        seen["stop"].as_str(),
        Some("STOP"),
        "one stop stays a string per the OpenAI wire spec"
    );
    assert_eq!(seen["user"].as_str(), Some("u-9"));
    assert_eq!(
        seen["tools"][0]["function"]["parameters"]["type"].as_str(),
        Some("object")
    );
    assert!(seen.get("system").is_none());
    assert!(seen.get("stop_sequences").is_none());
}

#[tokio::test]
async fn a_same_protocol_call_is_relayed_with_only_the_model_rewritten() {
    let mock = Mock::json(
        200,
        json!({"id":"m","type":"message","role":"assistant","model":"real-model",
               "content":[{"type":"text","text":"kept"}],"stop_reason":"end_turn",
               "usage":{"input_tokens":2,"output_tokens":2}}),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/messages",
        json!({"model": "alias", "max_tokens": 64, "temperature": 0.2,
               "messages": [{"role": "user", "content": "hi"}], "top_k": 3}),
        &[],
    )
    .await;

    assert_eq!(body["content"][0]["text"].as_str(), Some("kept"));
    let seen = mock.last_request();
    assert_eq!(seen["model"].as_str(), Some("real-model"));
    assert_eq!(
        seen["top_k"].as_u64(),
        Some(3),
        "same-protocol params survive"
    );
    assert_eq!(seen["temperature"].as_f64(), Some(0.2));
}

#[tokio::test]
async fn client_keys_gate_access_and_never_leak_to_the_upstream_body() {
    let mut config = {
        let mock = Mock::json(
            200,
            json!({"id":"m","type":"message","role":"assistant","model":"real-model",
                   "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn",
                   "usage":{"input_tokens":1,"output_tokens":1}}),
        );
        let addr = spawn_mock(mock).await;
        let mut config = config_for(addr, Protocol::Anthropic);
        config.server.master_keys = vec!["pk-live".into()];
        config
    };
    config.providers[0].api_key = "upstream-secret".into();
    let proxy = Proxy::start(config).await.unwrap();
    let body =
        json!({"model": "alias", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]});

    let (status, rejected, _) = post_json(&proxy, "/v1/messages", body.clone(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(rejected["type"].as_str(), Some("error"));
    assert_eq!(
        rejected["error"]["type"].as_str(),
        Some("authentication_error")
    );

    let (status, _, _) = post_json(
        &proxy,
        "/v1/messages",
        body.clone(),
        &[("x-api-key", "pk-wrong")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = post_json(&proxy, "/v1/messages", body, &[("x-api-key", "pk-live")]).await;
    assert_eq!(status, StatusCode::OK);

    let openai_shape = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "alias", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(openai_shape.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        openai_shape.1["error"]["type"].as_str(),
        Some("authentication_error")
    );
}

#[tokio::test]
async fn an_unknown_model_is_a_four_oh_four_in_the_clients_protocol() {
    let (_proxy, _mock) = proxy_for(Protocol::Anthropic).await;
    let proxy = &_proxy;

    let (status, body, _) = post_json(
        proxy,
        "/v1/messages",
        json!({"model": "totally-unknown", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["type"].as_str(), Some("not_found_error"));
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("totally-unknown"));
}

#[tokio::test]
async fn an_upstream_failure_keeps_its_status_and_words() {
    let mock = Mock::json(
        429,
        json!({"type":"error","error":{"type":"rate_limit_error","message":"Too many requests, retry in 3s"}}),
    );
    let addr = spawn_mock(mock).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let (status, body, raw) = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "alias", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body["error"]["message"].as_str(),
        Some("Too many requests, retry in 3s")
    );
    assert_eq!(body["error"]["type"].as_str(), Some("rate_limit_error"));
    assert!(
        raw.contains("rate_limit_error"),
        "the client sees an OpenAI-shaped envelope"
    );

    let logged = proxy.state.logs.recent(4);
    let entry = logged
        .iter()
        .find(|entry| entry.status == 429)
        .expect("the failure is logged");
    assert!(
        entry
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("rate_limit_error"),
        "the raw upstream body is kept for the log panel"
    );
}

#[tokio::test]
async fn requests_land_in_the_log_with_usage_and_drop_notes() {
    let plain = Mock::json(
        200,
        json!({"id":"m","type":"message","role":"assistant","model":"real-model",
               "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn",
               "usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":2}}),
    );
    let streamed = Mock::streaming(
        ANTHROPIC_REPLY_FRAMES
            .iter()
            .map(|frame| frame.to_string())
            .collect(),
    );
    let plain_addr = spawn_mock(plain).await;
    let streamed_addr = spawn_mock(streamed).await;

    let mut config = config_for(plain_addr, Protocol::Anthropic);
    config.providers.push(Provider {
        name: "stream".into(),
        protocol: Protocol::Anthropic,
        api: None,
        base_url: format!("http://{streamed_addr}"),
        patterns: vec!["live*".into()],
        models: vec![],
        ..Default::default()
    });
    let proxy = Proxy::start(config).await.unwrap();

    post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "alias", "messages": [{"role": "user", "content": "hi"}], "presence_penalty": 1.0}),
        &[],
    )
    .await;
    post_stream(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "live-claude", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;

    let logged = proxy.state.logs.recent(10);
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[0].requested_model, "live-claude".to_string());
    assert_eq!(logged[0].provider, "stream".to_string());
    assert!(logged[0].streaming);
    assert_eq!(logged[0].output_tokens, 7);
    assert!(logged[0].first_token_ms.is_some());

    let plain_entry = &logged[1];
    assert_eq!(plain_entry.provider, "mock".to_string());
    assert_eq!(
        plain_entry.upstream_protocol,
        "anthropic/messages".to_string()
    );
    assert_eq!(plain_entry.requested_model, "alias".to_string());
    assert_eq!(plain_entry.upstream_model, "real-model".to_string());
    assert!(!plain_entry.streaming);
    assert!(plain_entry.converted);
    assert_eq!(plain_entry.input_tokens, 5);
    assert_eq!(plain_entry.output_tokens, 4);
    assert!(plain_entry.latency_ms > 0);
    assert!(plain_entry
        .dropped
        .iter()
        .any(|d| d.contains("presence_penalty")));

    let stats = proxy.state.logs.stats();
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.errors, 0);
    assert_eq!(stats.by_provider["mock"].requests, 1);
    assert_eq!(stats.by_provider["stream"].requests, 1);
}

#[tokio::test]
async fn config_hot_reload_changes_routing_without_a_restart() {
    let openai_mock = Mock::json(
        200,
        json!({"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"real-model",
               "choices":[{"index":0,"message":{"role":"assistant","content":"from openai"},"finish_reason":"stop"}],
               "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
    );
    let anthropic_mock = Mock::json(
        200,
        json!({"id":"msg-x","type":"message","role":"assistant","model":"real-model",
               "content":[{"type":"text","text":"from anthropic"}],"stop_reason":"end_turn",
               "usage":{"input_tokens":1,"output_tokens":1}}),
    );
    let openai_addr = spawn_mock(openai_mock).await;
    let anthropic_addr = spawn_mock(anthropic_mock).await;

    let mut config = config_for(openai_addr, Protocol::Openai);
    config.providers.push(Provider {
        name: "other".into(),
        protocol: Protocol::Anthropic,
        api: None,
        base_url: format!("http://{anthropic_addr}"),
        patterns: vec!["claude*".into()],
        models: vec![],
        ..Default::default()
    });
    let proxy = Proxy::start(config.clone()).await.unwrap();

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(
        body["choices"][0]["message"]["content"].as_str(),
        Some("from openai")
    );

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "other/real-model", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(
        body["choices"][0]["message"]["content"].as_str(),
        Some("from anthropic")
    );

    let mut reloaded = config.clone();
    reloaded.providers[0].patterns = vec!["gpt*".into(), "claude*".into()];
    reloaded.providers[0].api_key = "swapped".into();
    proxy.apply(Arc::new(reloaded));

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/chat/completions",
        json!({"model": "claude-sonnet-4-5", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(
        body["choices"][0]["message"]["content"].as_str(),
        Some("from openai"),
        "claude models now resolve to the reloaded first provider"
    );
}

#[tokio::test]
async fn the_models_endpoint_lists_configured_aliases() {
    let (proxy, _mock) = proxy_for(Protocol::Openai).await;
    let response = reqwest::Client::new()
        .get(url(&proxy, "/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["object"].as_str(), Some("list"));
    assert_eq!(body["data"][0]["id"].as_str(), Some("alias"));
    assert_eq!(body["data"][0]["owned_by"].as_str(), Some("mock"));

    let health: Value = reqwest::Client::new()
        .get(url(&proxy, "/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"].as_str(), Some("ok"));
    assert_eq!(health["providers"].as_u64(), Some(1));
}

#[tokio::test]
async fn an_invalid_body_is_rejected_in_the_clients_shape() {
    let (proxy, mock) = proxy_for(Protocol::Openai).await;
    let response = reqwest::Client::new()
        .post(url(&proxy, "/v1/chat/completions"))
        .header("content-type", "application/json")
        .body("{\"model\": 42,")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("invalid_request_error")
    );
    assert_eq!(mock.requests(), 0, "the upstream is never called");

    let logged = proxy.state.logs.recent(5);
    assert_eq!(logged[0].status, 400);
    assert_eq!(logged[0].notes[0], "rejected before routing".to_string());
}

/// A Responses SSE frame with the envelope fields filled in.
fn rev(sequence: u64, kind: &str, body: Value) -> String {
    let mut payload = body.as_object().cloned().unwrap_or_default();
    payload.insert("type".into(), json!(kind));
    payload.insert("sequence_number".into(), json!(sequence));
    format!("data: {}\n\n", Value::Object(payload))
}

fn responses_upstream_frames() -> Vec<String> {
    vec![
        rev(
            1,
            "response.created",
            json!({"response": {"id": "resp_1", "object": "response",
            "created_at": 1, "status": "in_progress", "model": "real-model", "output": []}}),
        ),
        rev(
            2,
            "response.in_progress",
            json!({"response": {"id": "resp_1", "object": "response",
            "created_at": 1, "status": "in_progress", "model": "real-model", "output": []}}),
        ),
        rev(
            3,
            "response.output_item.added",
            json!({"output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}}),
        ),
        rev(
            4,
            "response.content_part.added",
            json!({"item_id": "msg_1", "output_index": 0,
            "content_index": 0, "part": {"type": "output_text", "text": ""}}),
        ),
        rev(
            5,
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0,
            "content_index": 0, "delta": "Hel"}),
        ),
        rev(
            6,
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0,
            "content_index": 0, "delta": "lo"}),
        ),
        rev(
            7,
            "response.output_text.done",
            json!({"item_id": "msg_1", "output_index": 0,
            "content_index": 0, "text": "Hello"}),
        ),
        rev(
            8,
            "response.content_part.done",
            json!({"item_id": "msg_1", "output_index": 0,
            "content_index": 0, "part": {"type": "output_text", "text": "Hello", "annotations": []}}),
        ),
        rev(
            9,
            "response.output_item.done",
            json!({"output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                      "content": [{"type": "output_text", "text": "Hello", "annotations": []}]}}),
        ),
        rev(
            10,
            "response.output_item.added",
            json!({"output_index": 1,
            "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get_time", "arguments": ""}}),
        ),
        rev(
            11,
            "response.function_call_arguments.delta",
            json!({"item_id": "fc_1",
            "output_index": 1, "delta": "{\"a\""}),
        ),
        rev(
            12,
            "response.function_call_arguments.delta",
            json!({"item_id": "fc_1",
            "output_index": 1, "delta": ":1}"}),
        ),
        rev(
            13,
            "response.function_call_arguments.done",
            json!({"item_id": "fc_1",
            "output_index": 1, "arguments": "{\"a\":1}"}),
        ),
        rev(
            14,
            "response.output_item.done",
            json!({"output_index": 1,
            "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get_time",
                      "arguments": "{\"a\":1}", "status": "completed"}}),
        ),
        rev(
            15,
            "response.completed",
            json!({"response": {"id": "resp_1", "object": "response",
            "created_at": 1, "status": "completed", "model": "real-model", "output": [],
            "usage": {"input_tokens": 9, "output_tokens": 4, "total_tokens": 13,
                       "input_tokens_details": {"cached_tokens": 2}}}}),
        ),
        "data: [DONE]\n\n".to_string(),
    ]
}

fn responses_provider_config(addr: SocketAddr) -> Config {
    let mut config = config_for(addr, Protocol::Openai);
    config.providers[0].api = Some(proxy::Api::Responses);
    config
}

#[tokio::test]
async fn responses_client_against_an_anthropic_upstream() {
    let mock = Mock::json(
        200,
        json!({"id": "msg_5", "type": "message", "role": "assistant", "model": "real-model",
               "content": [{"type": "text", "text": "answer"},
                            {"type": "tool_use", "id": "toolu_5", "name": "get_time", "input": {"city": "Oslo"}}],
               "stop_reason": "tool_use",
               "usage": {"input_tokens": 6, "output_tokens": 5, "cache_read_input_tokens": 2}}),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let (status, body, _) = post_json(
        &proxy,
        "/v1/responses",
        json!({
            "model": "alias",
            "instructions": "be terse",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "time?"}]},
                {"type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "get.time", "arguments": "{\"city\":\"Oslo\"}"},
                {"type": "function_call_output", "call_id": "call_9", "output": "22:00"}
            ],
            "tools": [{"type": "function", "name": "get.time", "parameters": {"type": "object"}}],
            "max_output_tokens": 200
        }),
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["object"].as_str(), Some("response"));
    assert_eq!(body["status"].as_str(), Some("completed"));
    assert_eq!(body["output"][0]["type"].as_str(), Some("message"));
    assert_eq!(
        body["output"][0]["content"][0]["type"].as_str(),
        Some("output_text")
    );
    assert_eq!(
        body["output"][0]["content"][0]["text"].as_str(),
        Some("answer")
    );
    assert_eq!(body["output"][1]["type"].as_str(), Some("function_call"));
    assert_eq!(body["output"][1]["name"].as_str(), Some("get.time"));
    assert_eq!(body["output"][1]["call_id"].as_str(), Some("toolu_5"));
    assert_eq!(body["usage"]["input_tokens"].as_u64(), Some(8));
    assert_eq!(body["usage"]["output_tokens"].as_u64(), Some(5));
    assert!(
        body.get("output_text").is_none(),
        "output_text is an SDK helper, not wire data"
    );

    let seen = mock.last_request();
    assert_eq!(seen["model"].as_str(), Some("real-model"));
    assert_eq!(seen["system"][0]["text"].as_str(), Some("be terse"));
    assert_eq!(seen["max_tokens"].as_u64(), Some(200));
    assert_eq!(seen["tools"][0]["name"].as_str(), Some("get_time"));
    assert_eq!(
        seen["messages"][1]["content"][0]["type"].as_str(),
        Some("tool_use")
    );
    assert_eq!(
        seen["messages"][1]["content"][0]["name"].as_str(),
        Some("get_time")
    );
    assert_eq!(
        seen["messages"][2]["content"][0]["type"].as_str(),
        Some("tool_result")
    );
}

#[tokio::test]
async fn responses_client_streaming_from_an_anthropic_upstream() {
    let mock = Mock::streaming(
        ANTHROPIC_REPLY_FRAMES
            .iter()
            .map(|frame| frame.to_string())
            .collect(),
    );
    let addr = spawn_mock(mock).await;
    let proxy = Proxy::start(config_for(addr, Protocol::Anthropic))
        .await
        .unwrap();

    let text = post_stream(
        &proxy,
        "/v1/responses",
        json!({
            "model": "alias",
            "stream": true,
            "input": "hi",
            "tools": [{"type": "function", "name": "get.time", "parameters": {"type": "object"}}]
        }),
    )
    .await;

    let names: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("event: "))
        .collect();
    assert_eq!(
        names,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(text.ends_with("data: [DONE]\n\n"));

    let deltas: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| data.starts_with('{'))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter(|value| value["type"] == json!("response.output_text.delta"))
        .map(|value| value["delta"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);

    let completed: Value = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find(|value| value["type"] == json!("response.completed"))
        .expect("a completion frame");
    assert_eq!(completed["response"]["status"].as_str(), Some("completed"));
    assert_eq!(
        completed["response"]["usage"]["output_tokens"].as_u64(),
        Some(7)
    );
    assert_eq!(
        completed["response"]["usage"]["input_tokens"].as_u64(),
        Some(12)
    );
    let items = completed["response"]["output"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["name"].as_str(), Some("get.time"));
    assert_eq!(items[1]["arguments"].as_str(), Some(r#"{"city": "Oslo"}"#));

    let numbers: Vec<u64> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter_map(|value| value["sequence_number"].as_u64())
        .collect();
    assert_eq!(numbers, (1..=numbers.len() as u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn openai_client_against_an_openai_responses_upstream() {
    let mock = Mock::streaming(responses_upstream_frames());
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(responses_provider_config(addr)).await.unwrap();

    let text = post_stream(
        &proxy,
        "/v1/chat/completions",
        json!({
            "model": "alias",
            "stream": true,
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": "hi"}
            ],
            "tools": [{"type": "function", "function": {"name": "get.time", "parameters": {"type": "object"}}}]
        }),
    )
    .await;

    let chunks = frames_to_text(&text);
    let content: String = chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(content, "Hello".to_string());

    let arguments: String = chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|call| {
            call.pointer("/function/arguments")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    assert_eq!(arguments, r#"{"a":1}"#.to_string());

    let named: Vec<&str> = chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/tool_calls/0/function/name")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(named, vec!["get.time"]);

    let usage = chunks
        .iter()
        .find(|chunk| chunk.get("usage").is_some())
        .expect("a usage frame");
    assert_eq!(usage["usage"]["prompt_tokens"].as_u64(), Some(9));
    assert_eq!(usage["usage"]["completion_tokens"].as_u64(), Some(4));
    assert!(text.ends_with("data: [DONE]\n\n"));

    let seen = mock.last_request();
    assert_eq!(seen["model"].as_str(), Some("real-model"));
    assert_eq!(seen["instructions"].as_str(), Some("be terse"));
    assert_eq!(seen["max_output_tokens"].as_u64(), Some(4096));
    assert_eq!(seen["input"][0]["type"].as_str(), Some("message"));
    assert_eq!(
        seen["input"][0]["content"].as_str(),
        Some("hi"),
        "a single text part stays a plain string, which Responses accepts"
    );
    assert_eq!(seen["tools"][0]["name"].as_str(), Some("get_time"));
    assert!(
        seen["tools"][0].get("function").is_none(),
        "Responses tools are flat"
    );
}

#[tokio::test]
async fn messages_client_against_an_openai_responses_upstream_bridges_two_hops() {
    let mock = Mock::streaming(responses_upstream_frames());
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(responses_provider_config(addr)).await.unwrap();

    let text = post_stream(
        &proxy,
        "/v1/messages",
        json!({
            "model": "alias",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;

    let names: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("event: "))
        .collect();
    assert_eq!(names[0], "message_start");
    assert_eq!(names[1], "content_block_start");
    assert_eq!(
        names[2..5],
        [
            "content_block_delta",
            "content_block_delta",
            "content_block_stop"
        ]
    );
    assert_eq!(names[5], "content_block_start");
    assert_eq!(
        names[names.len() - 3..],
        ["content_block_stop", "message_delta", "message_stop"]
    );
    assert!(!text.contains("[DONE]"));

    let seen = mock.last_request();
    assert_eq!(seen["max_output_tokens"].as_u64(), Some(64));
    assert_eq!(seen["input"][0]["role"].as_str(), Some("user"));
    assert_eq!(
        seen["input"][0]["content"].as_str(),
        Some("hi"),
        "a single text part stays a plain string, which Responses accepts"
    );
}

#[tokio::test]
async fn a_responses_call_to_a_responses_upstream_keeps_native_params() {
    let mock = Mock::json(
        200,
        json!({"id": "resp_9", "object": "response", "created_at": 2, "status": "completed",
               "model": "real-model",
               "output": [{"type": "message", "id": "msg_9", "role": "assistant", "status": "completed",
                            "content": [{"type": "output_text", "text": "kept", "annotations": []}]}],
               "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}),
    );
    let addr = spawn_mock(mock.clone()).await;
    let proxy = Proxy::start(responses_provider_config(addr)).await.unwrap();

    let (_status, body, _) = post_json(
        &proxy,
        "/v1/responses",
        json!({
            "model": "alias",
            "input": "hi",
            "reasoning": {"effort": "low", "summary": "auto"},
            "store": false,
            "truncation": "auto"
        }),
        &[],
    )
    .await;

    assert_eq!(
        body["output"][0]["content"][0]["text"].as_str(),
        Some("kept")
    );
    let seen = mock.last_request();
    assert_eq!(seen["reasoning"]["summary"].as_str(), Some("auto"));
    assert_eq!(seen["store"].as_bool(), Some(false));
    assert_eq!(seen["truncation"].as_str(), Some("auto"));
    assert_eq!(seen["model"].as_str(), Some("real-model"));
}

#[tokio::test]
async fn a_stateful_responses_request_is_refused_with_a_clear_error() {
    let (proxy, _mock) = proxy_for(Protocol::Anthropic).await;

    let (status, body, _) = post_json(
        &proxy,
        "/v1/responses",
        json!({"model": "alias", "input": "hi", "previous_response_id": "resp_old"}),
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("invalid_request_error")
    );
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("previous_response_id"));
}

#[tokio::test]
async fn every_surface_reports_errors_in_its_own_envelope() {
    let (proxy, _mock) = proxy_for(Protocol::Anthropic).await;

    let (_, body, _) = post_json(
        &proxy,
        "/v1/responses",
        json!({"model": "no-such-model", "input": "hi"}),
        &[],
    )
    .await;
    assert_eq!(body["error"]["type"].as_str(), Some("not_found_error"));

    let (_, body, _) = post_json(
        &proxy,
        "/v1/messages",
        json!({"model": "no-such-model", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(body["type"].as_str(), Some("error"));
    assert_eq!(body["error"]["type"].as_str(), Some("not_found_error"));
}
