use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub use crate::common::{CacheControl, Extras, OneOrMany};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<ImageUrl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            part_type: "text".into(),
            text: Some(text.into()),
            image_url: None,
            refusal: None,
            input_audio: None,
            file: None,
            cache_control: None,
            extras: Extras::new(),
        }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        Self {
            part_type: "image_url".into(),
            text: None,
            image_url: Some(ImageUrl {
                url: url.into(),
                detail: None,
                extras: Extras::new(),
            }),
            refusal: None,
            input_audio: None,
            file: None,
            cache_control: None,
            extras: Extras::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_blocks: Option<Vec<ThinkingBlock>>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(MessageContent::Text(content.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            thinking_blocks: None,
            extras: Extras::new(),
        }
    }

    pub fn text_content(&self) -> String {
        match &self.content {
            None => String::new(),
            Some(MessageContent::Text(text)) => text.clone(),
            Some(MessageContent::Parts(parts)) => parts
                .iter()
                .filter(|p| p.part_type == "text")
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    #[serde(rename = "type", default)]
    pub block_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub call_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolCallFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatTool {
    #[serde(rename = "type", default = "chat_tool_type")]
    pub tool_type: String,
    pub function: ChatToolFunction,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn chat_tool_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatToolChoice {
    Mode(String),
    Function { function: ChatToolChoiceFunction },
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolChoiceFunction {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type", default = "response_format_type")]
    pub format_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn response_format_type() -> String {
    "text".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ChatToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn is_false(v: &bool) -> bool {
    !v
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            response_format: None,
            temperature: None,
            top_p: None,
            n: None,
            max_tokens: None,
            max_completion_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            reasoning_effort: None,
            user: None,
            seed: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            extras: Extras::new(),
        }
    }

    pub fn token_limit(&self) -> Option<u32> {
        self.max_tokens.or(self.max_completion_tokens)
    }

    pub fn wants_usage_chunk(&self) -> bool {
        self.stream_options
            .as_ref()
            .and_then(|o| o.include_usage)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_tokens: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<UsageDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<UsageDetails>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: usize,
    pub message: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_blocks: Option<Vec<ThinkingBlock>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub delta: Delta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ChatChunk {
    pub fn delta_chunk(&self, delta: Delta, finish_reason: Option<String>) -> Self {
        Self {
            id: self.id.clone(),
            object: self.object.clone(),
            created: self.created,
            model: self.model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason,
            }],
            usage: None,
            extras: Extras::new(),
        }
    }

    pub fn usage_chunk(&self, usage: ChatUsage) -> Self {
        Self {
            id: self.id.clone(),
            object: self.object.clone(),
            created: self.created,
            model: self.model.clone(),
            choices: Vec::new(),
            usage: Some(usage),
            extras: Extras::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatError {
    pub error: ChatErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCard {
    pub id: String,
    #[serde(default = "model_card_object")]
    pub object: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn model_card_object() -> String {
    "model".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelList {
    pub object: String,
    pub data: Vec<ModelCard>,
}

impl Default for ChatResponse {
    fn default() -> Self {
        Self {
            id: String::new(),
            object: "chat.completion".into(),
            created: 0,
            model: String::new(),
            choices: Vec::new(),
            usage: None,
            extras: Extras::new(),
        }
    }
}

impl Default for ChatChunk {
    fn default() -> Self {
        Self {
            id: String::new(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: String::new(),
            choices: Vec::new(),
            usage: None,
            extras: Extras::new(),
        }
    }
}

impl Default for ContentPart {
    fn default() -> Self {
        Self::text(String::new())
    }
}

impl Default for ChatMessage {
    fn default() -> Self {
        Self {
            role: String::new(),
            content: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            thinking_blocks: None,
            extras: Extras::new(),
        }
    }
}
