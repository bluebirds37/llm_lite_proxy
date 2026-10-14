use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::common::Extras;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputPayload {
    Text(String),
    Items(Vec<InputItem>),
}

impl Default for InputPayload {
    fn default() -> Self {
        Self::Items(Vec::new())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl InputContent {
    pub fn plain_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => parts
                .iter()
                .map(|part| part.text.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type", default)]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Value>>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ContentPart {
    pub fn input_text(text: impl Into<String>) -> Self {
        Self {
            part_type: "input_text".into(),
            text: Some(text.into()),
            image_url: None,
            file_url: None,
            file_data: None,
            filename: None,
            file_id: None,
            detail: None,
            refusal: None,
            annotations: None,
            extras: Extras::new(),
        }
    }

    pub fn output_text(text: impl Into<String>) -> Self {
        Self {
            part_type: "output_text".into(),
            ..Self::input_text(text)
        }
    }
}

/// One `input` array element. Message items, `function_call`,
/// `function_call_output` and `reasoning` all share this shape so an item type
/// this crate does not model still survives the round trip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InputItem {
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<InputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl InputItem {
    pub fn message(role: impl Into<String>, content: InputContent) -> Self {
        Self {
            item_type: "message".into(),
            role: Some(role.into()),
            content: Some(content),
            ..Default::default()
        }
    }

    pub fn function_call(
        id: Option<String>,
        call_id: Option<String>,
        name: &str,
        arguments: &str,
    ) -> Self {
        Self {
            item_type: "function_call".into(),
            id,
            call_id,
            name: Some(name.to_string()),
            arguments: Some(arguments.to_string()),
            ..Default::default()
        }
    }

    pub fn function_call_output(call_id: Option<String>, output: Value) -> Self {
        Self {
            item_type: "function_call_output".into(),
            call_id,
            output: Some(output),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesTool {
    #[serde(rename = "type", default = "function_tool_type")]
    pub tool_type: String,
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

fn function_tool_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    Mode(String),
    Named {
        #[serde(rename = "type", default)]
        choice_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextParam {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: InputPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ResponsesToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponsesUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<UsageDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<UsageDetails>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputItem {
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl OutputItem {
    pub fn message(id: impl Into<String>) -> Self {
        Self {
            item_type: "message".into(),
            id: Some(id.into()),
            role: Some("assistant".into()),
            status: Some("completed".into()),
            content: Some(Vec::new()),
            ..Default::default()
        }
    }

    pub fn reasoning(id: impl Into<String>) -> Self {
        Self {
            item_type: "reasoning".into(),
            id: Some(id.into()),
            summary: Some(Vec::new()),
            ..Default::default()
        }
    }

    pub fn function_call(id: impl Into<String>, call_id: impl Into<String>, name: &str) -> Self {
        Self {
            item_type: "function_call".into(),
            id: Some(id.into()),
            call_id: Some(call_id.into()),
            name: Some(name.to_string()),
            status: Some("completed".into()),
            arguments: Some(String::new()),
            ..Default::default()
        }
    }

    pub fn text_parts(&self) -> String {
        self.content
            .as_ref()
            .map(|parts| {
                parts
                    .iter()
                    .filter(|part| part.part_type == "output_text")
                    .map(|part| part.text.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default = "response_object_type")]
    pub object: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

fn response_object_type() -> String {
    "response".into()
}

impl Default for ResponsesResponse {
    fn default() -> Self {
        Self {
            id: String::new(),
            object: "response".into(),
            created_at: 0,
            status: "completed".into(),
            model: String::new(),
            output: Vec::new(),
            usage: None,
            error: None,
            incomplete_details: None,
            instructions: None,
            completed_at: None,
            sequence_number: None,
            extras: Extras::new(),
        }
    }
}

/// Every Responses SSE event shares one envelope, so an event type this crate
/// has not seen still parses and can be forwarded or skipped without failing
/// the whole stream.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponsesEvent {
    #[serde(rename = "type", default)]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponsesResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<OutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ResponsesEvent {
    pub fn new(event_type: impl Into<String>, sequence_number: u64) -> Self {
        Self {
            event_type: event_type.into(),
            sequence_number: Some(sequence_number),
            ..Default::default()
        }
    }

    pub fn with_response(mut self, response: ResponsesResponse) -> Self {
        self.response = Some(response);
        self
    }

    pub fn with_item(mut self, output_index: u32, item: OutputItem) -> Self {
        self.output_index = Some(output_index);
        self.item_id = item.id.clone();
        self.item = Some(item);
        self
    }

    pub fn with_delta(
        mut self,
        item_id: impl Into<String>,
        output_index: u32,
        delta: impl Into<String>,
    ) -> Self {
        self.item_id = Some(item_id.into());
        self.output_index = Some(output_index);
        self.delta = Some(delta.into());
        self
    }

    pub fn with_text(
        mut self,
        item_id: impl Into<String>,
        output_index: u32,
        text: impl Into<String>,
    ) -> Self {
        self.item_id = Some(item_id.into());
        self.output_index = Some(output_index);
        self.text = Some(text.into());
        self
    }

    pub fn with_part(mut self, part: ContentPart) -> Self {
        self.content_index = Some(0);
        self.part = Some(part);
        self
    }

    pub fn with_error(mut self, code: impl Into<String>, message: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self.message = Some(message.into());
        self
    }
}

/// Verbatim `type` values on the wire.
pub mod events {
    pub const CREATED: &str = "response.created";
    pub const IN_PROGRESS: &str = "response.in_progress";
    pub const COMPLETED: &str = "response.completed";
    pub const FAILED: &str = "response.failed";
    pub const INCOMPLETE: &str = "response.incomplete";
    pub const OUTPUT_ITEM_ADDED: &str = "response.output_item.added";
    pub const OUTPUT_ITEM_DONE: &str = "response.output_item.done";
    pub const CONTENT_PART_ADDED: &str = "response.content_part.added";
    pub const CONTENT_PART_DONE: &str = "response.content_part.done";
    pub const OUTPUT_TEXT_DELTA: &str = "response.output_text.delta";
    pub const OUTPUT_TEXT_DONE: &str = "response.output_text.done";
    pub const REASONING_SUMMARY_PART_ADDED: &str = "response.reasoning_summary_part.added";
    pub const REASONING_SUMMARY_TEXT_DELTA: &str = "response.reasoning_summary_text.delta";
    pub const REASONING_SUMMARY_TEXT_DONE: &str = "response.reasoning_summary_text.done";
    pub const REASONING_SUMMARY_PART_DONE: &str = "response.reasoning_summary_part.done";
    pub const FUNCTION_CALL_ARGS_DELTA: &str = "response.function_call_arguments.delta";
    pub const FUNCTION_CALL_ARGS_DONE: &str = "response.function_call_arguments.done";
    pub const ERROR: &str = "error";
}
