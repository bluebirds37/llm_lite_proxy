use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::common::{CacheControl, ContentSource, Extras};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<MessageParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<MessagesTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<MessagesToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemParam {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

impl SystemParam {
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type", default = "text_block_type")]
    pub block_type: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

fn text_block_type() -> String {
    "text".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: String,
    #[serde(default)]
    pub content: MessageContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<Block>),
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

impl MessageContent {
    pub fn blocks(&self) -> Vec<Block> {
        match self {
            Self::Text(text) => vec![Block::text(text.clone())],
            Self::Blocks(blocks) => blocks.clone(),
        }
    }

    pub fn plain_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter(|b| b.block_type == "text")
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

/// Flat, lenient content block. Every Anthropic block kind shares one shape so
/// that block types this crate does not model survive a round trip instead of
/// failing to deserialize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    #[serde(rename = "type", default)]
    pub block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ContentSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl Block {
    pub fn text(text: impl Into<String>) -> Self {
        Self::typed("text".into()).with_text(text)
    }

    pub fn typed(block_type: String) -> Self {
        Self {
            block_type,
            text: None,
            source: None,
            id: None,
            name: None,
            input: None,
            tool_use_id: None,
            content: None,
            is_error: None,
            thinking: None,
            signature: None,
            data: None,
            cache_control: None,
            extras: Extras::new(),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn text_value(&self) -> String {
        self.text.clone().unwrap_or_default()
    }

    pub fn thinking_value(&self) -> String {
        self.thinking.clone().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<Block>),
}

impl ToolResultContent {
    pub fn plain_text(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter(|b| b.block_type == "text")
                .map(|b| b.text_value())
                .reduce(|acc, text| acc + &text),
        }
    }

    pub fn is_only_text(&self) -> bool {
        match self {
            Self::Text(_) => true,
            Self::Blocks(blocks) => {
                !blocks.is_empty() && blocks.iter().all(|b| b.block_type == "text")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesToolChoice {
    #[serde(rename = "type")]
    pub choice_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesResponse {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "message_object_type")]
    pub object: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub content: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: MessagesUsage,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn message_object_type() -> String {
    "message".into()
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MessagesUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<Value>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // wire reality: unit events sit next to full message payloads
pub enum StreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: MessagesUsage,
    },
    ContentBlockStart {
        index: u32,
        content_block: Block,
    },
    ContentBlockDelta {
        index: u32,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageStop,
    Ping,
    Error {
        error: ErrorBody,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] // an unparseable event keeps its raw payload
pub enum IncomingEvent {
    Known(StreamEvent),
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    #[serde(rename = "type", default)]
    pub error_type: String,
    #[serde(default)]
    pub message: String,
}
