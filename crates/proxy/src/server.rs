use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::{FromRequest, Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    CONTENT_TYPE, ORIGIN,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures::stream::{unfold, BoxStream, StreamExt};
use serde_json::Value;

use crate::config::{Api, Config, Server};
use crate::error::{Kind, ProxyError};
use crate::logs::{LogBus, StreamedTrace, Trace, TraceHandle};
use crate::routing::{self, Target};
use crate::upstream::{self, Admitted};

pub const CHAT_ENDPOINT: &str = "/v1/chat/completions";
pub const RESPONSES_ENDPOINT: &str = "/v1/responses";
pub const MESSAGES_ENDPOINT: &str = "/v1/messages";

pub const fn endpoint_of(api: Api) -> &'static str {
    match api {
        Api::Chat => CHAT_ENDPOINT,
        Api::Responses => RESPONSES_ENDPOINT,
        Api::Messages => MESSAGES_ENDPOINT,
    }
}

pub struct ProxyState {
    pub config: ArcSwap<Config>,
    pub client: reqwest::Client,
    pub logs: LogBus,
}

pub type Shared = Arc<ProxyState>;

pub fn build_state(config: Config) -> Shared {
    let capacity = config.server.log_capacity;
    build_state_with(config, LogBus::new(capacity))
}

pub fn build_state_with(config: Config, logs: LogBus) -> Shared {
    Arc::new(ProxyState {
        logs,
        client: reqwest_client(),
        config: ArcSwap::from_pointee(config),
    })
}

pub fn reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .user_agent(concat!("llm-lite-proxy/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("the default client always builds")
}

pub fn router(state: Shared) -> Router {
    let limit = state
        .config
        .load()
        .server
        .max_body_mb
        .saturating_mul(1024 * 1024) as usize;

    Router::new()
        .route(CHAT_ENDPOINT, post(handle_chat))
        .route(RESPONSES_ENDPOINT, post(handle_responses))
        .route(MESSAGES_ENDPOINT, post(handle_messages))
        .route("/v1/models", get(list_models))
        .route("/health", get(health))
        .layer(middleware::from_fn_with_state(Arc::clone(&state), cors))
        .layer(axum::extract::DefaultBodyLimit::max(limit))
        .with_state(state)
}

fn origin_allowed(allow: &[String], origin: &str) -> bool {
    if allow.iter().any(|entry| entry == "*") {
        return true;
    }
    !origin.is_empty()
        && allow
            .iter()
            .any(|entry| entry.trim_end_matches('/') == origin.trim_end_matches('/'))
}

fn allow_origin(headers: &mut HeaderMap, origin: &str) {
    let value = HeaderValue::from_str(origin).unwrap_or_else(|_| HeaderValue::from_static("*"));
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, value);
}

async fn cors(State(state): State<Shared>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let allowed = origin_allowed(&state.config.load().server.allow_origins, &origin);

    if allowed && request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        allow_origin(response.headers_mut(), &origin);
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST, GET, OPTIONS"),
        );
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "content-type, authorization, x-api-key, anthropic-version, anthropic-beta",
            ),
        );
        return response;
    }

    let mut response = next.run(request).await;
    if allowed {
        allow_origin(response.headers_mut(), &origin);
    }
    response
}

struct RawBody(Vec<u8>);

impl<S> FromRequest<S> for RawBody
where
    S: Send + Sync,
{
    type Rejection = ProxyError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = <Bytes as FromRequest<S>>::from_request(request, state)
            .await
            .map_err(|error| {
                ProxyError::bad_request(format!("the request body could not be read: {error}"))
            })?;
        Ok(Self(bytes.to_vec()))
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        // Extractors run before the route is known, so the Chat shape is used.
        self.into_response_for(Api::Chat)
    }
}

enum Inbound {
    Chat(protocol::ChatRequest),
    Responses(protocol::ResponsesRequest),
    Messages(protocol::MessagesRequest),
}

impl Inbound {
    fn parse(api: Api, body: &[u8]) -> Result<Self, ProxyError> {
        let rejected = |error: serde_json::Error| {
            ProxyError::bad_request(format!("invalid {} request: {error}", api.label()))
        };
        match api {
            Api::Chat => serde_json::from_slice(body)
                .map(Self::Chat)
                .map_err(rejected),
            Api::Responses => serde_json::from_slice(body)
                .map(Self::Responses)
                .map_err(rejected),
            Api::Messages => serde_json::from_slice(body)
                .map(Self::Messages)
                .map_err(rejected),
        }
    }

    fn model(&self) -> String {
        match self {
            Self::Chat(request) => request.model.clone(),
            Self::Responses(request) => request.model.clone(),
            Self::Messages(request) => request.model.clone(),
        }
    }

    fn streaming(&self) -> bool {
        match self {
            Self::Chat(request) => request.stream,
            Self::Responses(request) => request.stream,
            Self::Messages(request) => request.stream,
        }
    }
}

/// What gets posted upstream, plus the two name maps a reply has to pass
/// through: `upstream_tools` undoes what the request conversion sanitised, and
/// `client_tools` restores what the client originally called its tools.
struct Outbound {
    body: Vec<u8>,
    upstream_tools: protocol::ToolNameMap,
    client_tools: protocol::ToolNameMap,
    dropped: Vec<String>,
}

/// Chat Completions is the only internal representation, so each surface needs
/// one bridge to it rather than a bridge to every other surface.
fn build_outbound(
    config: &Config,
    client: Api,
    target: &Target<'_>,
    inbound: Inbound,
) -> Result<Outbound, ProxyError> {
    let options = config.options();
    let upstream_api = target.provider.api();

    // Same surface on both sides: only the model name changes, so nothing that
    // this crate does not model can be lost on the way through.
    if client == upstream_api {
        let body = match inbound {
            Inbound::Chat(mut request) => {
                request.model = target.model.clone();
                serde_json::to_vec(&request)
            }
            Inbound::Responses(mut request) => {
                request.model = target.model.clone();
                serde_json::to_vec(&request)
            }
            Inbound::Messages(mut request) => {
                request.model = target.model.clone();
                serde_json::to_vec(&request)
            }
        }
        .map_err(|error| ProxyError::internal(error.to_string()))?;
        return Ok(Outbound {
            body,
            upstream_tools: protocol::ToolNameMap::new(),
            client_tools: protocol::ToolNameMap::new(),
            dropped: Vec::new(),
        });
    }
    let (mut chat, client_tools, mut dropped) = match inbound {
        Inbound::Chat(request) => (request, protocol::ToolNameMap::new(), Vec::new()),
        Inbound::Responses(request) => {
            let converted = protocol::responses_request_to_chat(&request, &options)?;
            (converted.request, converted.tools, converted.dropped)
        }
        Inbound::Messages(request) => {
            let converted = protocol::messages_request_to_chat(&request, &options)?;
            (converted.request, converted.tools, converted.dropped)
        }
    };
    chat.model = target.model.clone();

    let (body, upstream_tools) = match upstream_api {
        Api::Chat => (serde_json::to_vec(&chat), protocol::ToolNameMap::new()),
        Api::Responses => {
            let converted = protocol::chat_request_to_responses(&chat, &options)?;
            dropped.extend(converted.dropped);
            (serde_json::to_vec(&converted.request), converted.tools)
        }
        Api::Messages => {
            let converted = protocol::chat_request_to_messages(&chat, &options)?;
            dropped.extend(converted.dropped);
            (serde_json::to_vec(&converted.request), converted.tools)
        }
    };

    Ok(Outbound {
        body: body.map_err(|error| ProxyError::internal(error.to_string()))?,
        upstream_tools,
        client_tools,
        dropped,
    })
}

async fn handle_chat(State(state): State<Shared>, headers: HeaderMap, body: RawBody) -> Response {
    dispatch(&state, Api::Chat, CHAT_ENDPOINT, &headers, &body.0).await
}

async fn handle_responses(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: RawBody,
) -> Response {
    dispatch(
        &state,
        Api::Responses,
        RESPONSES_ENDPOINT,
        &headers,
        &body.0,
    )
    .await
}

async fn handle_messages(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: RawBody,
) -> Response {
    dispatch(&state, Api::Messages, MESSAGES_ENDPOINT, &headers, &body.0).await
}

async fn health(State(state): State<Shared>) -> Json<Value> {
    let config = state.config.load();
    Json(serde_json::json!({
        "status": "ok",
        "providers": config.providers.iter().filter(|provider| provider.enabled).count(),
        "active": state.logs.stats().active,
    }))
}

async fn list_models(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(error) = authorize(&state, &headers) {
        return error.into_response_for(Api::Chat);
    }
    let data = routing::public_models(&state.config.load())
        .into_iter()
        .map(|(name, provider)| protocol::ModelCard {
            id: name,
            object: "model".into(),
            created: Some(protocol::created()),
            owned_by: Some(provider),
            extras: Default::default(),
        })
        .collect::<Vec<_>>();

    Json(serde_json::json!({"object": "list", "data": data})).into_response()
}

fn authorize(state: &Shared, headers: &HeaderMap) -> Result<(), ProxyError> {
    let config = state.config.load();
    let configured: Vec<&str> = config
        .server
        .master_keys
        .iter()
        .map(String::as_str)
        .filter(|key| !key.trim().is_empty())
        .collect();
    if configured.is_empty() {
        return Ok(());
    }
    match upstream::forwarded_key(headers) {
        Some(supplied) if configured.contains(&supplied.as_str()) => Ok(()),
        Some(_) => Err(ProxyError::unauthorized("this proxy key is not allowed")),
        None => Err(ProxyError::unauthorized(
            "send Authorization: Bearer <key> or x-api-key: <key>",
        )),
    }
}

async fn dispatch(
    state: &Shared,
    client: Api,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if let Err(error) = authorize(state, headers) {
        state
            .logs
            .record_rejected(client, endpoint, 401, &error.message);
        return error.into_response_for(client);
    }

    let config = state.config.load_full();
    let inbound = match Inbound::parse(client, body) {
        Ok(inbound) => inbound,
        Err(error) => {
            state
                .logs
                .record_rejected(client, endpoint, 400, &error.message);
            return error.into_response_for(client);
        }
    };
    let requested_model = inbound.model();
    let wants_stream = inbound.streaming();

    let trace = Trace::new(state.logs.clone(), client, endpoint, &requested_model);
    let handle = trace.handle();
    if wants_stream {
        handle.streaming();
    }

    let target = match routing::resolve(&config, &requested_model) {
        Ok(target) => target,
        Err(error) => {
            trace.error(&error);
            return error.into_response_for(client);
        }
    };
    let upstream_api = target.provider.api();
    handle.route(
        &target.provider.name,
        &format!("{}/{}", target.provider.protocol, upstream_api),
        &target.model,
        client != upstream_api,
    );

    let outbound = match build_outbound(&config, client, &target, inbound) {
        Ok(outbound) => outbound,
        Err(error) => {
            trace.error(&error);
            return error.into_response_for(client);
        }
    };
    handle.dropped(&outbound.dropped);

    let key = upstream::resolve_key(target.provider, headers);
    let prepared = upstream::build(
        &state.client,
        target.provider,
        key.as_deref(),
        outbound.body,
        headers,
        (!wants_stream).then(|| Duration::from_secs(config.server.request_timeout_secs.max(1))),
    );

    let admitted = match upstream::send(prepared, &handle).await {
        Ok(admitted) => admitted,
        Err(error) => return error.into_response_for(client),
    };

    let route = Route {
        client,
        upstream_api,
        upstream_tools: outbound.upstream_tools.clone(),
        client_tools: outbound.client_tools.clone(),
    };

    match (admitted, wants_stream) {
        (Admitted::Stream(stream), true) => {
            let transformer = Transformer::new(
                (client == upstream_api).then_some(client),
                upstream_api,
                client,
                outbound.upstream_tools,
                outbound.client_tools,
            );
            let (handle, streamed) = trace.with_completed();
            sse_response(
                stream,
                transformer,
                streamed,
                handle,
                Duration::from_secs(config.server.stream_idle_timeout_secs.max(1)),
            )
        }
        (admitted, streaming_wanted) => {
            let outcome = read_whole(admitted, &config.server).await;
            respond_whole(outcome, route, streaming_wanted, trace, &handle)
        }
    }
}

struct Route {
    client: Api,
    upstream_api: Api,
    upstream_tools: protocol::ToolNameMap,
    client_tools: protocol::ToolNameMap,
}

fn respond_whole(
    outcome: Result<Value, ProxyError>,
    route: Route,
    streaming_wanted: bool,
    trace: Trace,
    handle: &TraceHandle,
) -> Response {
    let body = match outcome {
        Ok(value) => reshape(value, &route),
        Err(error) => {
            trace.error(&error);
            return error.into_response_for(route.client);
        }
    };
    match body {
        Ok(body) => {
            let (input, output) = json_tokens(route.client, &body);
            handle.tokens(input, output);
            if streaming_wanted {
                handle.note("the upstream answered a streaming request with a plain JSON body");
            }
            trace.complete(200, None, None);
            Json(body).into_response()
        }
        Err(error) => {
            trace.error(&error);
            error.into_response_for(route.client)
        }
    }
}

fn reshape(value: Value, route: &Route) -> Result<Value, ProxyError> {
    if route.client == route.upstream_api {
        return Ok(value);
    }

    let chat = match route.upstream_api {
        Api::Chat => serde_json::from_value::<protocol::ChatResponse>(value)
            .map_err(|error| upstream_shape_error("Chat", &error))?,
        Api::Responses => {
            let response = serde_json::from_value::<protocol::ResponsesResponse>(value)
                .map_err(|error| upstream_shape_error("Responses", &error))?;
            protocol::responses_response_to_chat(response, &route.upstream_tools)
        }
        Api::Messages => {
            let response = serde_json::from_value::<protocol::MessagesResponse>(value)
                .map_err(|error| upstream_shape_error("Messages", &error))?;
            protocol::messages_response_to_chat(response, &route.upstream_tools)
        }
    };

    let body = match route.client {
        Api::Chat => serde_json::to_value(chat),
        Api::Responses => serde_json::to_value(protocol::chat_response_to_responses(
            chat,
            &route.client_tools,
        )),
        Api::Messages => serde_json::to_value(protocol::chat_response_to_messages(
            chat,
            &route.client_tools,
        )),
    }
    .map_err(|error| ProxyError::internal(error.to_string()))?;
    Ok(body)
}

fn upstream_shape_error(name: &str, error: &serde_json::Error) -> ProxyError {
    ProxyError::internal(format!("the upstream is not a {name} response: {error}"))
}

async fn read_whole(admitted: Admitted, server: &Server) -> Result<Value, ProxyError> {
    let text = match admitted {
        Admitted::Json(value) => return Ok(value),
        Admitted::Stream(stream) => {
            collect(
                stream,
                Duration::from_secs(server.request_timeout_secs.max(1)),
            )
            .await?
        }
    };
    serde_json::from_str(&text).map_err(|error| {
        ProxyError::internal(format!("the upstream body is not JSON: {error}")).with_detail(text)
    })
}

async fn collect(
    mut stream: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    timeout: Duration,
) -> Result<String, ProxyError> {
    let reading = tokio::time::timeout(timeout, async {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|error| ProxyError::internal(error.to_string()))?;
            out.extend_from_slice(&bytes);
        }
        Ok::<Vec<u8>, ProxyError>(out)
    })
    .await;
    match reading {
        Ok(result) => result.map(|bytes| String::from_utf8_lossy(&bytes).to_string()),
        Err(_) => Err(ProxyError::new(
            StatusCode::GATEWAY_TIMEOUT,
            Kind::Timeout,
            "the upstream body stalled",
        )),
    }
}

fn json_tokens(api: Api, body: &Value) -> (u64, u64) {
    let usage = body.get("usage");
    let number = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or_default()
    };
    match api {
        Api::Chat => (number("prompt_tokens"), number("completion_tokens")),
        Api::Responses => {
            let cached = usage
                .and_then(|usage| usage.get("input_tokens_details"))
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            (number("input_tokens") + cached, number("output_tokens"))
        }
        Api::Messages => (
            number("input_tokens")
                + number("cache_read_input_tokens")
                + number("cache_creation_input_tokens"),
            number("output_tokens"),
        ),
    }
}

enum Decoder {
    Chat,
    Messages(protocol::AnthropicToChatStream),
    Responses(protocol::ResponsesToChatStream),
}

enum Encoder {
    Chat,
    Messages(protocol::ChatToAnthropicStream),
    Responses(protocol::ChatToResponsesStream),
}

/// Frames in, frames out. When the client and the upstream already speak the
/// same surface the frames are relayed untouched.
pub struct Transformer {
    relay: Option<Api>,
    decoder: Decoder,
    encoder: Encoder,
    done: bool,
    skipped: usize,
    input_tokens: u64,
    output_tokens: u64,
}

impl Transformer {
    fn new(
        relay: Option<Api>,
        upstream_api: Api,
        client: Api,
        upstream_tools: protocol::ToolNameMap,
        client_tools: protocol::ToolNameMap,
    ) -> Self {
        let decoder = match upstream_api {
            Api::Chat => Decoder::Chat,
            Api::Messages => {
                Decoder::Messages(protocol::AnthropicToChatStream::new("", upstream_tools))
            }
            Api::Responses => {
                Decoder::Responses(protocol::ResponsesToChatStream::new("", upstream_tools))
            }
        };
        let encoder = match client {
            Api::Chat => Encoder::Chat,
            Api::Messages => {
                Encoder::Messages(protocol::ChatToAnthropicStream::new("", client_tools))
            }
            Api::Responses => {
                Encoder::Responses(protocol::ChatToResponsesStream::new("", client_tools))
            }
        };
        Self {
            relay,
            decoder,
            encoder,
            done: false,
            skipped: 0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn on_frame(&mut self, frame: &protocol::SseFrame) -> Vec<String> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            // Only a relayed [DONE] reaches the client; a converted stream owes
            // its own terminator regardless of what the upstream sent.
            self.done = self.relay.is_some();
            return Vec::new();
        }

        let encoded = match self.relay {
            Some(api) => {
                self.note_relay_usage(api, data);
                vec![frame_for(api, frame)]
            }
            None => {
                let mut out = Vec::new();
                for chunk in self.decode(data) {
                    out.extend(self.encode(&chunk));
                }
                out
            }
        };
        self.note_tokens();
        encoded
    }

    fn decode(&mut self, data: &str) -> Vec<protocol::ChatChunk> {
        match &mut self.decoder {
            Decoder::Chat => match serde_json::from_str::<protocol::ChatChunk>(data) {
                Ok(chunk) => {
                    // The chat decoder is the only stateless one, so its usage
                    // frame has to be read here or the log entry loses tokens.
                    if let Some(usage) = chunk.usage.as_ref() {
                        self.input_tokens = self.input_tokens.max(usage.prompt_tokens);
                        self.output_tokens = self.output_tokens.max(usage.completion_tokens);
                    }
                    vec![chunk]
                }
                Err(_) => Vec::new(),
            },
            Decoder::Messages(converter) => match serde_json::from_str(data) {
                Ok(protocol::IncomingEvent::Known(event)) => converter.push(&event),
                Ok(protocol::IncomingEvent::Raw(value)) => {
                    self.skipped += 1;
                    let _ = value;
                    Vec::new()
                }
                Err(_) => {
                    self.skipped += 1;
                    Vec::new()
                }
            },
            Decoder::Responses(converter) => match serde_json::from_str(data) {
                Ok(event) => converter.push(&event),
                Err(_) => {
                    self.skipped += 1;
                    Vec::new()
                }
            },
        }
    }

    fn encode(&mut self, chunk: &protocol::ChatChunk) -> Vec<String> {
        match &mut self.encoder {
            Encoder::Chat => vec![protocol::encode_openai(&json_of(chunk))],
            Encoder::Messages(converter) => converter.push(chunk).iter().map(event_frame).collect(),
            Encoder::Responses(converter) => converter
                .push(chunk)
                .iter()
                .map(response_event_frame)
                .collect(),
        }
    }

    fn note_tokens(&mut self) {
        let (input, output) = match &self.decoder {
            Decoder::Chat => return,
            Decoder::Messages(converter) => converter.token_counts(),
            Decoder::Responses(converter) => converter.token_counts(),
        };
        self.input_tokens = self.input_tokens.max(input);
        self.output_tokens = self.output_tokens.max(output);
    }

    /// A relayed stream is the only case where the tokens were never parsed, so
    /// the usage frame is read just for the log.
    fn note_relay_usage(&mut self, api: Api, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let number = |cursor: Option<&Value>, key: &str| {
            cursor
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let (input, output) = match api {
            Api::Chat => (
                number(value.get("usage"), "prompt_tokens").max(self.input_tokens),
                number(value.get("usage"), "completion_tokens").max(self.output_tokens),
            ),
            Api::Responses => {
                let usage = value.pointer("/response/usage");
                (
                    number(usage, "input_tokens").max(self.input_tokens),
                    number(usage, "output_tokens").max(self.output_tokens),
                )
            }
            Api::Messages => {
                let usage = value
                    .pointer("/message/usage")
                    .or_else(|| value.pointer("/usage"));
                (
                    (number(usage, "input_tokens")
                        + number(usage, "cache_read_input_tokens")
                        + number(usage, "cache_creation_input_tokens"))
                    .max(self.input_tokens),
                    number(usage, "output_tokens").max(self.output_tokens),
                )
            }
        };
        self.input_tokens = input;
        self.output_tokens = output;
    }

    fn finish(&mut self) -> Vec<String> {
        let mut out = if self.relay.is_some() {
            Vec::new()
        } else {
            let mut frames = Vec::new();
            let chunks = match &mut self.decoder {
                Decoder::Chat => Vec::new(),
                Decoder::Messages(converter) => converter.finish(),
                Decoder::Responses(converter) => converter.finish(),
            };
            for chunk in &chunks {
                frames.extend(self.encode(chunk));
            }
            frames.extend(match &mut self.encoder {
                Encoder::Chat => Vec::new(),
                Encoder::Messages(converter) => {
                    converter.finish().iter().map(event_frame).collect()
                }
                Encoder::Responses(converter) => converter
                    .finish()
                    .iter()
                    .map(response_event_frame)
                    .collect(),
            });
            frames
        };
        if !self.done && self.client_surface() != Api::Messages {
            out.push(protocol::DONE_FRAME.to_string());
        }
        self.note_tokens();
        out
    }

    fn client_surface(&self) -> Api {
        match self.relay {
            Some(api) => api,
            None => match self.encoder {
                Encoder::Chat => Api::Chat,
                Encoder::Messages(_) => Api::Messages,
                Encoder::Responses(_) => Api::Responses,
            },
        }
    }

    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

fn frame_for(api: Api, frame: &protocol::SseFrame) -> String {
    match api {
        Api::Chat | Api::Responses => protocol::encode_openai(&frame.data),
        Api::Messages => protocol::encode_frame(frame.event.as_deref(), &frame.data),
    }
}

fn event_frame(event: &protocol::StreamEvent) -> String {
    let name = match event {
        protocol::StreamEvent::MessageStart { .. } => "message_start",
        protocol::StreamEvent::MessageDelta { .. } => "message_delta",
        protocol::StreamEvent::ContentBlockStart { .. } => "content_block_start",
        protocol::StreamEvent::ContentBlockDelta { .. } => "content_block_delta",
        protocol::StreamEvent::ContentBlockStop { .. } => "content_block_stop",
        protocol::StreamEvent::MessageStop => "message_stop",
        protocol::StreamEvent::Ping => "ping",
        protocol::StreamEvent::Error { .. } => "error",
    };
    protocol::encode_frame(Some(name), &json_of(event))
}

fn response_event_frame(event: &protocol::ResponsesEvent) -> String {
    protocol::encode_frame(Some(&event.event_type), &json_of(event))
}

fn json_of<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
}

struct Pipe {
    upstream: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queue: VecDeque<Bytes>,
    transformer: Transformer,
    handle: TraceHandle,
    idle: Duration,
    finished: bool,
    trace: Option<StreamedTrace>,
}

impl Pipe {
    async fn step(&mut self) -> Option<Bytes> {
        loop {
            if let Some(next) = self.queue.pop_front() {
                self.handle.first_token();
                return Some(next);
            }
            if self.finished {
                return None;
            }
            self.pump().await;
        }
    }

    async fn pump(&mut self) {
        match tokio::time::timeout(self.idle, self.upstream.next()).await {
            Err(_) => self.failed("the upstream stream stalled"),
            Ok(None) => self.end(StatusCode::OK, None),
            Ok(Some(Err(error))) => {
                self.failed(&format!("the upstream stream failed: {error}"));
            }
            Ok(Some(Ok(bytes))) => {
                self.buffer.extend_from_slice(&bytes);
                for frame in protocol::take_frames(&mut self.buffer) {
                    let frames = self.transformer.on_frame(&frame);
                    self.queue.extend(frames.into_iter().map(Bytes::from));
                }
                self.report_tokens();
            }
        }
    }

    fn end(&mut self, status: StatusCode, error: Option<String>) {
        if self.finished {
            return;
        }
        let trailer = self.transformer.finish();
        if self.transformer.skipped() > 0 {
            self.handle.note(&format!(
                "{} upstream frames were not understood",
                self.transformer.skipped()
            ));
        }
        self.queue.extend(trailer.into_iter().map(Bytes::from));
        self.finished = true;
        self.report_tokens();
        if let Some(trace) = self.trace.take() {
            trace.complete(status.as_u16(), error, None);
        }
    }

    fn failed(&mut self, message: &str) {
        self.end(StatusCode::BAD_GATEWAY, Some(message.to_string()));
    }

    fn report_tokens(&mut self) {
        self.handle.tokens(
            self.transformer.input_tokens,
            self.transformer.output_tokens,
        );
    }
}

fn sse_response(
    upstream: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    transformer: Transformer,
    trace: StreamedTrace,
    handle: TraceHandle,
    idle: Duration,
) -> Response {
    let pipe = Pipe {
        upstream,
        buffer: Vec::with_capacity(16 * 1024),
        queue: VecDeque::new(),
        transformer,
        handle,
        idle,
        finished: false,
        trace: Some(trace),
    };

    let body = Body::from_stream(
        unfold(pipe, |mut pipe| async move {
            pipe.step().await.map(|frame| (frame, pipe))
        })
        .map(Ok::<Bytes, Infallible>),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .unwrap_or_else(|_| {
            ProxyError::internal("could not build the SSE response").into_response_for(Api::Chat)
        })
}
