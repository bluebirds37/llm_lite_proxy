use serde_json::{Map, Value};

use crate::anthropic::{
    Block, ContentSource, MessageContent as AnthropicContent, MessageParam, MessagesRequest,
    MessagesResponse, MessagesTool, MessagesToolChoice, Metadata, SystemBlock, SystemParam,
    ThinkingConfig, ToolResultContent,
};
use crate::common::{dedup_notices, merge_extras, source_from_data_url, Extras, OneOrMany};
use crate::error::ConversionError;
use crate::ids::{chat_id, created};
use crate::openai::{
    ChatMessage, ChatRequest, ChatResponse, ChatToolChoice, Choice, ContentPart, MessageContent,
    ThinkingBlock, ToolCall, ToolCallFunction,
};
use crate::reasons::finish_reason_from_stop_reason;
use crate::tools::{sanitize_id, sanitize_name, ToolNameMap, ANTHROPIC_ID_MAX, ANTHROPIC_NAME_MAX};
use crate::usage::chat_usage_from_messages;
use crate::{Converted, Options};

const MESSAGES_FIELDS: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "system",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "tools",
    "tool_choice",
    "thinking",
    "metadata",
    "service_tier",
];

const NO_MESSAGES_EQUIVALENT: &[&str] = &[
    "presence_penalty",
    "frequency_penalty",
    "logit_bias",
    "seed",
    "logprobs",
    "top_logprobs",
    "stream_options",
    "audio",
    "prediction",
    "modalities",
    "web_search_options",
];

pub fn chat_request_to_messages(
    request: &ChatRequest,
    options: &Options,
) -> Result<Converted<MessagesRequest>, ConversionError> {
    let mut tools = ToolNameMap::new();
    let mut dropped = Vec::new();

    if request.n.unwrap_or(1) > 1 {
        return Err(ConversionError::unsupported(
            "n",
            "the Messages API",
            "it returns a single completion per request",
        ));
    }

    let (system, rest) = split_leading_system(&request.messages);
    let messages = build_messages(&request.messages, rest, &mut tools, &mut dropped);
    if messages.is_empty() {
        return Err(ConversionError::EmptyConversation);
    }

    let thinking = reasoning_effort_to_thinking(request.reasoning_effort.as_deref(), &mut dropped);
    let mut max_tokens = request.token_limit().unwrap_or(options.default_max_tokens);
    if let Some(budget) = thinking.as_ref().and_then(|config| config.budget_tokens) {
        max_tokens = max_tokens.max(budget + 1_024);
    }

    let temperature = match (thinking.is_some(), request.temperature) {
        (true, Some(value)) if (value - 1.0).abs() > f32::EPSILON => {
            dropped.push("temperature (Anthropic pins it to 1 while thinking is on)".into());
            None
        }
        _ => request.temperature,
    };

    for field in NO_MESSAGES_EQUIVALENT {
        if present_in_chat_request(request, field) {
            dropped.push(format!("{field} (no Messages equivalent)"));
        }
    }

    let declared_tools = request.tools.as_ref().map(|declared| {
        declared
            .iter()
            .map(|tool| {
                let upstream = sanitize_name(&tool.function.name, ANTHROPIC_NAME_MAX);
                tools.register(&tool.function.name, &upstream);
                if upstream.len() > 64 {
                    dropped.push(format!(
                        "tool name `{}` shortened to `{upstream}` by the 64 character limit",
                        tool.function.name
                    ));
                }
                MessagesTool {
                    name: upstream,
                    description: tool.function.description.clone(),
                    input_schema: Some(tool.function.parameters.clone().unwrap_or_else(
                        || serde_json::json!({"type": "object", "properties": {}}),
                    )),
                    cache_control: None,
                    extras: Extras::new(),
                }
            })
            .collect::<Vec<_>>()
    });

    let mut tool_choice = request
        .tool_choice
        .as_ref()
        .map(|choice| chat_tool_choice_to_messages(choice, &tools, &mut dropped));
    if request.parallel_tool_calls == Some(false) {
        tool_choice = Some(match tool_choice {
            Some(choice) => MessagesToolChoice {
                disable_parallel_tool_use: Some(true),
                ..choice
            },
            None => MessagesToolChoice {
                choice_type: "auto".into(),
                name: None,
                disable_parallel_tool_use: Some(true),
                extras: Extras::new(),
            },
        });
    }

    let mut extras = Extras::new();
    if let Some(schema) = json_schema_of(request) {
        extras.insert(
            "output_format".into(),
            serde_json::json!({"type": "json_schema", "schema": force_open_schema(&schema)}),
        );
    } else if request
        .response_format
        .as_ref()
        .is_some_and(|format| format.format_type == "json_object")
    {
        extras.insert(
            "output_format".into(),
            serde_json::json!({"type": "json_object"}),
        );
    }

    if options.passthrough_extras {
        merge_extras(&mut extras, &request.extras, MESSAGES_FIELDS);
    }

    dedup_notices(&mut dropped);

    Ok(Converted {
        tools,
        dropped,
        request: MessagesRequest {
            model: request.model.clone(),
            messages,
            max_tokens: Some(max_tokens),
            system: (!system.is_empty()).then_some(SystemParam::Blocks(system)),
            temperature,
            top_p: request.top_p,
            top_k: None,
            stop_sequences: request.stop.as_ref().map(|stop| match stop {
                OneOrMany::One(value) => vec![value.clone()],
                OneOrMany::Many(values) => values.clone(),
            }),
            stream: request.stream,
            tools: declared_tools,
            tool_choice,
            thinking,
            metadata: request.user.as_ref().map(|user_id| Metadata {
                user_id: Some(user_id.clone()),
                extras: Extras::new(),
            }),
            service_tier: None,
            extras,
        },
    })
}

fn present_in_chat_request(request: &ChatRequest, field: &str) -> bool {
    let numeric = |value: Option<f32>| value.is_some_and(|value| value != 0.0);
    match field {
        "presence_penalty" => numeric(request.presence_penalty),
        "frequency_penalty" => numeric(request.frequency_penalty),
        "seed" => request.seed.is_some(),
        "logprobs" => request.logprobs == Some(true),
        "top_logprobs" => request.top_logprobs.is_some_and(|value| value > 0),
        "logit_bias" => request
            .logit_bias
            .as_ref()
            .is_some_and(|bias| !bias.is_empty()),
        "stream_options" => request.stream_options.is_some(),
        _ => request.extras.contains_key(field),
    }
}

fn split_leading_system(messages: &[ChatMessage]) -> (Vec<SystemBlock>, usize) {
    let mut blocks = Vec::new();
    let mut end = 0;
    for (index, message) in messages.iter().enumerate() {
        if !matches!(message.role.as_str(), "system" | "developer") {
            break;
        }
        let text = message.text_content();
        if !text.is_empty() {
            let cache_control = match &message.content {
                Some(MessageContent::Parts(parts)) => {
                    parts.iter().find_map(|part| part.cache_control.clone())
                }
                _ => None,
            };
            blocks.push(SystemBlock {
                block_type: "text".into(),
                text,
                cache_control,
            });
        }
        end = index + 1;
    }
    (blocks, end)
}

#[derive(Default)]
struct MessageBuilder {
    messages: Vec<MessageParam>,
    pending_role: Option<String>,
    pending_blocks: Vec<Block>,
}

impl MessageBuilder {
    fn push(&mut self, role: &str, mut blocks: Vec<Block>) {
        blocks.retain(|block| !(block.block_type == "text" && block.text_value().is_empty()));
        if blocks.is_empty() {
            return;
        }
        if self.pending_role.as_deref() == Some(role) {
            self.pending_blocks.append(&mut blocks);
        } else {
            self.flush();
            self.pending_role = Some(role.to_string());
            self.pending_blocks = blocks;
        }
    }

    fn flush(&mut self) {
        let Some(role) = self.pending_role.take() else {
            return;
        };
        let blocks = std::mem::take(&mut self.pending_blocks);
        let (results, rest): (Vec<Block>, Vec<Block>) = blocks
            .into_iter()
            .partition(|block| block.block_type == "tool_result");
        self.messages.push(MessageParam {
            role,
            content: AnthropicContent::Blocks(results.into_iter().chain(rest).collect()),
        });
    }

    fn finish(mut self) -> Vec<MessageParam> {
        self.flush();
        self.messages
    }
}

fn build_messages(
    messages: &[ChatMessage],
    from: usize,
    tools: &mut ToolNameMap,
    dropped: &mut Vec<String>,
) -> Vec<MessageParam> {
    let mut builder = MessageBuilder::default();
    for message in &messages[from..] {
        match message.role.as_str() {
            "user" => builder.push("user", content_to_blocks(message, dropped)),
            "tool" | "function" => builder.push("user", vec![tool_result_block(message, dropped)]),
            "assistant" => builder.push("assistant", assistant_blocks(message, tools, dropped)),
            "system" | "developer" => {
                builder.push("user", vec![Block::text(message.text_content())])
            }
            other => dropped.push(format!("role `{other}`")),
        }
    }
    builder.finish()
}

fn content_to_blocks(message: &ChatMessage, dropped: &mut Vec<String>) -> Vec<Block> {
    match &message.content {
        None => Vec::new(),
        Some(MessageContent::Text(text)) => vec![Block::text(text.clone())],
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|part| part_to_block(part, dropped))
            .collect(),
    }
}

fn part_to_block(part: &ContentPart, dropped: &mut Vec<String>) -> Option<Block> {
    let block = match part.part_type.as_str() {
        "text" => {
            let mut block = Block::text(part.text.clone().unwrap_or_default());
            block.cache_control = part.cache_control.clone();
            block
        }
        "image_url" => {
            let url = part.image_url.as_ref()?.url.clone();
            let mut block = Block::typed("image".into());
            block.source = Some(source_from_data_url(&url));
            block.cache_control = part.cache_control.clone();
            block
        }
        "file" => {
            let file = part.file.as_ref()?;
            let mut source = file
                .get("file_data")
                .and_then(Value::as_str)
                .map(source_from_data_url)
                .unwrap_or_else(|| ContentSource {
                    source_type: "file".into(),
                    media_type: None,
                    data: None,
                    url: None,
                    file_id: file
                        .get("file_id")
                        .and_then(Value::as_str)
                        .map(String::from),
                    extras: Extras::new(),
                });
            if source.source_type == "base64" && source.media_type.is_none() {
                source.media_type = Some("application/pdf".into());
            }
            let mut block = Block::typed("document".into());
            block.source = Some(source);
            if let Some(title) = file.get("filename").and_then(Value::as_str) {
                block
                    .extras
                    .insert("title".into(), Value::String(title.to_string()));
            }
            block
        }
        other => {
            dropped.push(format!("content part `{other}`"));
            return None;
        }
    };
    Some(block)
}

fn tool_result_block(message: &ChatMessage, dropped: &mut Vec<String>) -> Block {
    let mut block = Block::typed("tool_result".into());
    let id = message
        .tool_call_id
        .clone()
        .or_else(|| message.name.clone());
    match id {
        Some(id) => block.tool_use_id = Some(sanitize_id(&id, ANTHROPIC_ID_MAX)),
        None => {
            block.tool_use_id = Some(sanitize_id(&chat_id(), ANTHROPIC_ID_MAX));
            dropped.push("tool message without tool_call_id got a synthetic id".into());
        }
    }
    block.content = match &message.content {
        None => Some(ToolResultContent::Text(String::new())),
        Some(MessageContent::Text(text)) => Some(ToolResultContent::Text(text.clone())),
        Some(MessageContent::Parts(parts)) => Some(ToolResultContent::Blocks(
            parts
                .iter()
                .filter_map(|part| part_to_block(part, dropped))
                .collect(),
        )),
    };
    block.is_error = message.extras.get("is_error").and_then(Value::as_bool);
    block
}

fn assistant_blocks(
    message: &ChatMessage,
    tools: &mut ToolNameMap,
    dropped: &mut Vec<String>,
) -> Vec<Block> {
    let mut blocks = Vec::new();

    if let Some(known) = &message.thinking_blocks {
        for block in known {
            blocks.push(match block.block_type.as_deref() {
                Some("redacted_thinking") => {
                    let mut out = Block::typed("redacted_thinking".into());
                    out.data = block.data.clone();
                    out
                }
                _ => {
                    let mut out = Block::typed("thinking".into());
                    out.thinking = Some(block.thinking.clone().unwrap_or_default());
                    out.signature = block.signature.clone();
                    out
                }
            });
        }
    } else if message
        .reasoning_content
        .as_deref()
        .is_some_and(|text| !text.is_empty())
    {
        dropped.push("reasoning_content (Anthropic rejects unsigned thinking blocks)".into());
    }

    let text = message.text_content();
    if !text.is_empty() {
        blocks.push(Block::text(text));
    }

    for call in message.tool_calls.clone().unwrap_or_default() {
        let function = call.function.clone().unwrap_or_default();
        let name = function.name.clone().unwrap_or_default();
        let upstream = sanitize_name(&name, ANTHROPIC_NAME_MAX);
        tools.register(&name, &upstream);
        let mut block = Block::typed("tool_use".into());
        block.id = Some(sanitize_id(
            &call.id.clone().unwrap_or_else(chat_id),
            ANTHROPIC_ID_MAX,
        ));
        block.name = Some(upstream);
        block.input = Some(parse_tool_arguments(function.arguments.as_deref()));
        blocks.push(block);
    }

    blocks
}

pub(crate) fn parse_tool_arguments(arguments: Option<&str>) -> Value {
    arguments
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn chat_tool_choice_to_messages(
    choice: &ChatToolChoice,
    tools: &ToolNameMap,
    dropped: &mut Vec<String>,
) -> MessagesToolChoice {
    let (mode, name) = match choice {
        ChatToolChoice::Mode(mode) => (mode.as_str(), None),
        ChatToolChoice::Function { function } => ("tool", Some(function.name.as_str())),
        ChatToolChoice::Other(value) => {
            let name = value
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .or_else(|| value.get("name"))
                .and_then(Value::as_str);
            (
                value.get("type").and_then(Value::as_str).unwrap_or("auto"),
                name,
            )
        }
    };

    let choice_type = match mode {
        "required" | "any" => "any",
        "none" => "none",
        "function" | "tool" => "tool",
        "auto" => "auto",
        other => {
            dropped.push(format!("tool_choice `{other}`"));
            "auto"
        }
    };

    MessagesToolChoice {
        choice_type: choice_type.to_string(),
        name: name.map(|name| tools.upstream_name(name).to_string()),
        disable_parallel_tool_use: None,
        extras: Extras::new(),
    }
}

fn reasoning_effort_to_thinking(
    effort: Option<&str>,
    dropped: &mut Vec<String>,
) -> Option<ThinkingConfig> {
    let lowered = effort.map(|value| value.to_ascii_lowercase());
    let budget = match lowered.as_deref() {
        None | Some("none") => return None,
        Some("minimal") => 1_024,
        Some("low") => 2_048,
        Some("medium") => 8_192,
        Some("high") => 16_384,
        Some("xhigh") => 32_768,
        Some(value) => {
            dropped.push(format!("reasoning_effort `{value}`"));
            return None;
        }
    };
    Some(ThinkingConfig {
        mode: "enabled".into(),
        budget_tokens: Some(budget),
    })
}

fn json_schema_of(request: &ChatRequest) -> Option<Value> {
    let format = request.response_format.as_ref()?;
    if format.format_type != "json_schema" {
        return None;
    }
    let schema = format.json_schema.as_ref()?;
    Some(
        schema
            .get("schema")
            .cloned()
            .unwrap_or_else(|| schema.clone()),
    )
}

/// Anthropic's structured output requires every declared key to be required and
/// rejects open objects, so a client schema has to be closed before it is sent.
pub(crate) fn force_open_schema(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut out: Map<String, Value> = object
                .iter()
                .map(|(key, nested)| (key.clone(), force_open_schema(nested)))
                .collect();
            let is_object = out
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "object")
                || out.contains_key("properties");
            if !is_object {
                return Value::Object(out);
            }
            out.insert("additionalProperties".into(), Value::Bool(false));
            let declared: Vec<String> = out
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            if declared.is_empty() {
                return Value::Object(out);
            }
            let mut required: Vec<String> = out
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            for key in declared {
                if !required.contains(&key) {
                    required.push(key);
                }
            }
            out.insert(
                "required".into(),
                Value::Array(required.into_iter().map(Value::String).collect()),
            );
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(force_open_schema).collect()),
        other => other.clone(),
    }
}

pub fn messages_response_to_chat(response: MessagesResponse, tools: &ToolNameMap) -> ChatResponse {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut thinking_blocks = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &response.content {
        match block.block_type.as_str() {
            "text" => text.push_str(&block.text_value()),
            "thinking" | "redacted_thinking" => {
                reasoning.push_str(&block.thinking_value());
                thinking_blocks.push(ThinkingBlock {
                    block_type: Some(block.block_type.clone()),
                    thinking: block.thinking.clone(),
                    signature: block.signature.clone(),
                    data: block.data.clone(),
                });
            }
            "tool_use" | "server_tool_use" => {
                tool_calls.push(ToolCall {
                    id: block.id.clone(),
                    call_id: None,
                    call_type: Some("function".into()),
                    function: Some(ToolCallFunction {
                        name: Some(
                            tools
                                .client_name(block.name.as_deref().unwrap_or(""))
                                .to_string(),
                        ),
                        arguments: Some(
                            block
                                .input
                                .clone()
                                .map(|input| input.to_string())
                                .unwrap_or_else(|| "{}".into()),
                        ),
                    }),
                    index: None,
                    extras: Extras::new(),
                });
            }
            _ => {}
        }
    }

    let message = ChatMessage {
        role: "assistant".into(),
        content: (!text.is_empty()).then_some(MessageContent::Text(text)),
        name: None,
        tool_call_id: None,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        thinking_blocks: (!thinking_blocks.is_empty()).then_some(thinking_blocks),
        extras: Extras::new(),
    };

    ChatResponse {
        id: if response.id.is_empty() {
            chat_id()
        } else {
            response.id
        },
        object: "chat.completion".into(),
        created: created(),
        model: if response.model.is_empty() {
            "unknown".into()
        } else {
            response.model
        },
        choices: vec![Choice {
            index: 0,
            finish_reason: Some(finish_reason_from_stop_reason(
                response.stop_reason.as_deref(),
            )),
            message,
            logprobs: None,
            extras: Extras::new(),
        }],
        usage: Some(chat_usage_from_messages(&response.usage, 0)),
        extras: Extras::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(body: Value) -> Converted<MessagesRequest> {
        chat_request_to_messages(&serde_json::from_value(body).unwrap(), &Options::default())
            .unwrap()
    }

    #[test]
    fn leading_system_becomes_top_level_system() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "developer", "content": "prefer rust"},
                {"role": "user", "content": "hi"}
            ]
        }));

        assert_eq!(
            converted.request.system.as_ref().unwrap().as_text(),
            "be terse\n\nprefer rust"
        );
        assert_eq!(converted.request.messages.len(), 1);
        assert_eq!(converted.request.max_tokens, Some(4096));
    }

    #[test]
    fn mid_conversation_system_folds_into_the_user_turn() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "system", "content": "note"},
                {"role": "user", "content": "two"}
            ]
        }));

        assert_eq!(converted.request.system, None);
        assert_eq!(converted.request.messages.len(), 1);
        assert_eq!(converted.request.messages[0].role, "user");
        assert_eq!(
            converted.request.messages[0].content.plain_text(),
            "onenotetwo"
        );
    }

    #[test]
    fn tool_calls_become_tool_use_with_a_reversible_name_map() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [
                {"role": "assistant", "content": "looking", "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get.time!", "arguments": "{\"city\":\"Oslo\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "22:00"}
            ],
            "tools": [{"type": "function", "function": {"name": "get.time!", "parameters": {"type": "object"}}}]
        }));

        let blocks = converted.request.messages[0].content.blocks();
        assert_eq!(blocks[0].block_type, "text");
        assert_eq!(blocks[1].block_type, "tool_use");
        assert_eq!(blocks[1].name.as_deref(), Some("get_time"));
        assert_eq!(blocks[1].input, Some(json!({"city": "Oslo"})));
        assert_eq!(converted.tools.client_name("get_time"), "get.time!");

        let results = converted.request.messages[1].content.blocks();
        assert_eq!(results[0].block_type, "tool_result");
        assert_eq!(results[0].tool_use_id.as_deref(), Some("call_1"));

        let tool = &converted.request.tools.as_ref().unwrap()[0];
        assert_eq!(tool.name, "get_time");
        assert_eq!(tool.input_schema, Some(json!({"type": "object"})));
    }

    #[test]
    fn malformed_tool_arguments_fall_back_to_an_empty_object() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "assistant", "tool_calls": [{
                "id": "call_1", "function": {"name": "run", "arguments": "{oops"}
            }]}]
        }));

        assert_eq!(
            converted.request.messages[0].content.blocks()[0].input,
            Some(json!({}))
        );
    }

    #[test]
    fn sampling_and_choice_fields_map_to_the_messages_names() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": {"type": "function", "function": {"name": "search"}},
            "stop": ["END", "STOP"],
            "max_tokens": 300,
            "presence_penalty": 0.5,
            "user": "u-1"
        }));

        let choice = converted.request.tool_choice.as_ref().unwrap();
        assert_eq!(choice.choice_type, "tool");
        assert_eq!(choice.name.as_deref(), Some("search"));
        assert_eq!(
            converted.request.stop_sequences.as_ref().unwrap(),
            &["END", "STOP"]
        );
        assert_eq!(converted.request.max_tokens, Some(300));
        assert_eq!(
            converted
                .request
                .metadata
                .as_ref()
                .unwrap()
                .user_id
                .as_deref(),
            Some("u-1")
        );
        assert!(converted
            .dropped
            .iter()
            .any(|d| d.starts_with("presence_penalty")));
    }

    #[test]
    fn required_tool_choice_and_no_parallel_tools_map_across() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": "required",
            "parallel_tool_calls": false
        }));

        let choice = converted.request.tool_choice.as_ref().unwrap();
        assert_eq!(choice.choice_type, "any");
        assert_eq!(choice.disable_parallel_tool_use, Some(true));
    }

    #[test]
    fn thinking_budget_raises_max_tokens_and_clears_temperature() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high",
            "max_tokens": 200,
            "temperature": 0.3
        }));

        assert_eq!(
            converted.request.thinking.as_ref().unwrap().budget_tokens,
            Some(16_384)
        );
        assert_eq!(converted.request.max_tokens, Some(17_408));
        assert_eq!(converted.request.temperature, None);
        assert!(converted
            .dropped
            .iter()
            .any(|d| d.starts_with("temperature")));
    }

    #[test]
    fn response_format_becomes_output_format_with_a_closed_schema() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {"type": "json_schema", "json_schema": {
                "name": "plan",
                "schema": {"type": "object", "properties": {"step": {"type": "string"}}}
            }}
        }));

        assert_eq!(
            converted.request.extras["output_format"],
            json!({"type": "json_schema", "schema": {
                "type": "object",
                "properties": {"step": {"type": "string"}},
                "additionalProperties": false,
                "required": ["step"]
            }})
        );
    }

    #[test]
    fn reasoning_content_without_a_signature_is_dropped_not_fabricated() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "答案", "reasoning_content": "private"}
            ]
        }));

        assert!(converted.request.messages[1]
            .content
            .blocks()
            .iter()
            .all(|b| b.block_type != "thinking"));
        assert!(converted
            .dropped
            .iter()
            .any(|d| d.starts_with("reasoning_content")));
    }

    #[test]
    fn multiple_completions_are_rejected() {
        let error = chat_request_to_messages(
            &serde_json::from_value(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "x"}],
                "n": 3
            }))
            .unwrap(),
            &Options::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("`n`"));
    }

    #[test]
    fn unknown_content_parts_are_reported_as_dropped() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "input_audio", "input_audio": {"data": "AAA", "format": "mp3"}}
            ]}]
        }));

        assert_eq!(converted.request.messages[0].content.blocks().len(), 1);
        assert!(converted.dropped.iter().any(|d| d.contains("input_audio")));
    }

    #[test]
    fn response_usage_folds_cache_buckets_into_prompt_tokens() {
        let response: MessagesResponse = serde_json::from_value(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 7, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 2}
        }))
        .unwrap();

        let chat = messages_response_to_chat(response, &ToolNameMap::new());
        let usage = chat.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(
            usage.prompt_tokens_details.as_ref().unwrap().cached_tokens,
            Some(3)
        );
        assert_eq!(chat.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn image_data_urls_round_trip_into_an_anthropic_source() {
        let converted = convert(json!({
            "model": "claude",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}
            ]}]
        }));

        let block = &converted.request.messages[0].content.blocks()[0];
        let source = block.source.as_ref().unwrap();
        assert_eq!(block.block_type, "image");
        assert_eq!(source.source_type, "base64");
        assert_eq!(source.media_type.as_deref(), Some("image/png"));
        assert_eq!(source.data.as_deref(), Some("QUJD"));
    }
}
