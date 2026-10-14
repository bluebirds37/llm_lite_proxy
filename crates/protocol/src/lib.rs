mod anthropic;
mod chat_to_messages;
mod chat_to_responses;
mod common;
mod error;
mod ids;
mod messages_to_chat;
mod openai;
mod openai_responses;
mod reasons;
mod stream;
mod tools;
mod usage;

pub use anthropic::{
    Block, BlockDelta, ContentSource, ErrorBody, IncomingEvent, MessageDeltaBody, MessageParam,
    MessagesRequest, MessagesResponse, MessagesTool, MessagesToolChoice, MessagesUsage, Metadata,
    StreamEvent, SystemBlock, SystemParam, ThinkingConfig, ToolResultContent,
};
pub use chat_to_messages::{chat_request_to_messages, messages_response_to_chat};
pub use chat_to_responses::{chat_request_to_responses, responses_response_to_chat};
pub use common::{CacheControl, Extras, OneOrMany};
pub use error::ConversionError;
pub use ids::{call_id, chat_id, created, message_id};
pub use messages_to_chat::{chat_response_to_messages, messages_request_to_chat};
pub use openai::{
    ChatChunk, ChatError, ChatErrorBody, ChatMessage, ChatRequest, ChatResponse, ChatTool,
    ChatToolChoice, ChatToolFunction, ChatUsage, Choice, ChunkChoice, ContentPart, Delta, ImageUrl,
    ModelCard, ModelList, ResponseFormat, StreamOptions, ThinkingBlock, ToolCall, ToolCallFunction,
    UsageDetails,
};
pub use openai_responses::events as response_events;
pub use openai_responses::{
    ContentPart as ResponsesContentPart, InputContent, InputItem, InputPayload, OutputItem,
    Reasoning, ResponsesEvent, ResponsesRequest, ResponsesResponse, ResponsesTool,
    ResponsesToolChoice, ResponsesUsage, TextParam, UsageDetails as ResponsesUsageDetails,
};
mod responses_to_chat;
pub use reasons::{finish_reason_from_stop_reason, stop_reason_from_finish};
pub use responses_to_chat::{
    chat_response_to_responses, finish_reason_from_status, responses_request_to_chat,
    status_from_finish,
};
pub use stream::{
    encode_frame, encode_openai, parse_frame, take_frames, AnthropicToChatStream,
    ChatToAnthropicStream, ChatToResponsesStream, ResponsesToChatStream, SseFrame, DONE_FRAME,
};
pub use tools::{sanitize_id, sanitize_name, ToolNameMap};
pub use usage::{chat_usage_from_messages, messages_usage_from_chat};

/// Tunables an operator may want to control without touching the conversion
/// code itself.
#[derive(Debug, Clone)]
pub struct Options {
    /// Anthropic rejects a request without `max_tokens`; a Chat Completions
    /// client may omit it, so the conversion supplies this floor.
    pub default_max_tokens: u32,
    /// Forward inbound keys this crate does not model (vendor extensions) to
    /// the outbound body instead of dropping them.
    pub passthrough_extras: bool,
    /// Ask an OpenAI upstream for a trailing usage-only chunk so token counts
    /// survive the trip into Anthropic's `message_delta`.
    pub openai_include_usage: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            default_max_tokens: 4096,
            passthrough_extras: false,
            openai_include_usage: true,
        }
    }
}

/// A converted upstream request, the tool-name map needed to undo the
/// sanitisation in the response, and everything the conversion gave up on.
#[derive(Debug, Clone)]
pub struct Converted<Request> {
    pub request: Request,
    pub tools: ToolNameMap,
    pub dropped: Vec<String>,
}
