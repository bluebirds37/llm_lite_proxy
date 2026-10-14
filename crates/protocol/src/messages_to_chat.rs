use serde_json::Value;

use crate::anthropic::{
    Block, MessagesRequest, MessagesResponse, MessagesToolChoice, SystemParam, ThinkingConfig,
    ToolResultContent,
};
use crate::common::{dedup_notices, merge_extras, source_to_url, Extras, OneOrMany};
use crate::error::ConversionError;
use crate::ids::{call_id, message_id};
use crate::openai::{
    ChatMessage, ChatRequest, ChatResponse, ChatTool, ChatToolChoice, ContentPart, MessageContent,
    StreamOptions, ThinkingBlock, ToolCall, ToolCallFunction,
};
use crate::reasons::stop_reason_from_finish;
use crate::tools::{sanitize_name, ToolNameMap, OPENAI_NAME_MAX};
use crate::usage::messages_usage_from_chat;
use crate::{anthropic, Converted, Options};

const CHAT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "temperature",
    "top_p",
    "n",
    "max_tokens",
    "max_completion_tokens",
    "presence_penalty",
    "frequency_penalty",
    "stop",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "user",
    "seed",
    "response_format",
    "reasoning_effort",
    "logit_bias",
    "logprobs",
    "top_logprobs",
];

pub fn messages_request_to_chat(
    request: &MessagesRequest,
    options: &Options,
) -> Result<Converted<ChatRequest>, ConversionError> {
    let mut tools = ToolNameMap::new();
    let mut dropped = Vec::new();
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(system) = request.system.as_ref().map(SystemParam::as_text) {
        if !system.is_empty() {
            messages.push(ChatMessage::text("system", system));
        }
    }

    for message in &request.messages {
        match message.role.as_str() {
            "user" => messages.extend(user_message_to_chat(message, &mut dropped)),
            "assistant" => {
                messages.push(assistant_message_to_chat(message, &mut tools, &mut dropped))
            }
            "system" | "developer" => {
                let text = message.content.plain_text();
                if !text.is_empty() {
                    messages.push(ChatMessage::text("system", text));
                }
            }
            other => dropped.push(format!("role `{other}`")),
        }
    }

    messages.retain(|message| !is_empty_assistant(message));
    if messages.is_empty() {
        return Err(ConversionError::EmptyConversation);
    }

    let declared_tools = request.tools.as_ref().map(|declared| {
        declared
            .iter()
            .map(|tool| {
                let upstream = sanitize_name(&tool.name, OPENAI_NAME_MAX);
                tools.register(&tool.name, &upstream);
                ChatTool {
                    tool_type: "function".into(),
                    function: crate::openai::ChatToolFunction {
                        name: upstream,
                        description: tool.description.clone(),
                        parameters: tool.input_schema.clone(),
                        strict: None,
                        extras: Extras::new(),
                    },
                    extras: tool.extras.clone(),
                }
            })
            .collect::<Vec<_>>()
    });

    let mut tool_choice = request
        .tool_choice
        .as_ref()
        .map(|choice| messages_tool_choice_to_chat(choice, &tools, &mut dropped));
    if request
        .tool_choice
        .as_ref()
        .and_then(|choice| choice.disable_parallel_tool_use)
        == Some(true)
    {
        tool_choice = tool_choice.or(Some(ChatToolChoice::Mode("auto".into())));
    }

    let mut extras = Extras::new();
    if let Some(output) = request
        .extras
        .get("output_format")
        .and_then(Value::as_object)
    {
        let kind = output
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("json_schema");
        if kind == "json_schema" {
            let schema = output
                .get("schema")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            extras.insert(
                "response_format".into(),
                serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {"name": "structured_output", "schema": schema, "strict": true}
                }),
            );
        } else {
            extras.insert(
                "response_format".into(),
                serde_json::json!({"type": "json_object"}),
            );
        }
    }

    if options.passthrough_extras {
        merge_extras(&mut extras, &request.extras, CHAT_FIELDS);
    }

    if request.top_k.is_some() {
        dropped.push("top_k (no Chat Completions equivalent)".into());
    }
    for field in [
        "service_tier",
        "inference_geo",
        "container",
        "context_management",
    ] {
        if request.extras.contains_key(field) {
            dropped.push(format!("{field} (no Chat Completions equivalent)"));
        }
    }
    let reasoning_effort = thinking_to_reasoning_effort(request.thinking.as_ref(), &mut dropped);
    dedup_notices(&mut dropped);

    Ok(Converted {
        tools,
        dropped,
        request: ChatRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            top_p: request.top_p,
            stop: request
                .stop_sequences
                .as_ref()
                .map(|stops| match stops.len() {
                    1 => OneOrMany::One(stops[0].clone()),
                    _ => OneOrMany::Many(stops.clone()),
                }),
            stream: request.stream,
            stream_options: (request.stream && options.openai_include_usage).then_some(
                StreamOptions {
                    include_usage: Some(true),
                    extras: Extras::new(),
                },
            ),
            tools: declared_tools,
            tool_choice,
            parallel_tool_calls: request
                .tool_choice
                .as_ref()
                .and_then(|choice| choice.disable_parallel_tool_use)
                .map(|disabled| !disabled),
            reasoning_effort,
            user: request
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.user_id.clone()),
            response_format: None,
            n: None,
            max_completion_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            extras,
        },
    })
}

fn is_empty_assistant(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && message
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty())
        && message.text_content().is_empty()
        && message
            .reasoning_content
            .as_deref()
            .unwrap_or_default()
            .is_empty()
}

fn user_message_to_chat(
    message: &anthropic::MessageParam,
    dropped: &mut Vec<String>,
) -> Vec<ChatMessage> {
    let mut results = Vec::new();
    let mut parts: Vec<ContentPart> = Vec::new();

    for block in message.content.blocks() {
        match block.block_type.as_str() {
            "tool_result" => results.push(tool_result_to_chat(&block, dropped)),
            "text" => parts.push(ContentPart::text(block.text_value())),
            "image" => {
                let source = match block.source {
                    Some(source) => source,
                    None => continue,
                };
                match source_to_url(&source) {
                    Some(url) => parts.push(ContentPart::image_url(url)),
                    None => dropped.push("image block with a file_id source".into()),
                }
            }
            "document" => {
                let source = match block.source {
                    Some(source) => source,
                    None => continue,
                };
                let filename = block
                    .extras
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("document")
                    .to_string();
                match source_to_url(&source) {
                    Some(file_data) => parts.push(ContentPart {
                        part_type: "file".into(),
                        text: None,
                        image_url: None,
                        refusal: None,
                        input_audio: None,
                        file: Some(serde_json::json!({
                            "filename": if filename.contains('.') { filename } else { format!("{filename}.pdf") },
                            "file_data": file_data
                        })),
                        cache_control: None,
                        extras: Extras::new(),
                    }),
                    None => dropped.push("document block with a file_id source".into()),
                }
            }
            "thinking" | "redacted_thinking" => dropped.push(block.block_type.clone()),
            _ => dropped.push(block.block_type.clone()),
        }
    }

    let mut out = results;
    if parts.iter().all(|part| part.part_type == "text") && !parts.is_empty() {
        let joined = parts
            .iter()
            .map(|part| part.text.clone().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("");
        out.push(ChatMessage::text("user", joined));
    } else if !parts.is_empty() {
        out.push(ChatMessage {
            role: "user".into(),
            content: Some(MessageContent::Parts(parts)),
            ..Default::default()
        });
    }
    out
}

fn tool_result_to_chat(block: &Block, dropped: &mut Vec<String>) -> ChatMessage {
    let content = match &block.content {
        None => String::new(),
        Some(ToolResultContent::Text(text)) => text.clone(),
        Some(ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .map(|nested| match nested.block_type.as_str() {
                "text" => nested.text_value(),
                other => {
                    dropped.push(format!("tool_result `{other}` content"));
                    String::new()
                }
            })
            .collect::<Vec<_>>()
            .join(""),
    };

    ChatMessage {
        role: "tool".into(),
        content: Some(MessageContent::Text(content)),
        tool_call_id: block.tool_use_id.clone(),
        extras: block
            .is_error
            .filter(|error| *error)
            .map(|_| {
                let mut extras = Extras::new();
                extras.insert("is_error".into(), Value::Bool(true));
                extras
            })
            .unwrap_or_default(),
        ..Default::default()
    }
}

fn assistant_message_to_chat(
    message: &anthropic::MessageParam,
    tools: &mut ToolNameMap,
    dropped: &mut Vec<String>,
) -> ChatMessage {
    let mut text = String::new();
    let mut thinking_blocks = Vec::new();
    let mut tool_calls = Vec::new();

    for block in message.content.blocks() {
        match block.block_type.as_str() {
            "text" => text.push_str(&block.text_value()),
            "thinking" | "redacted_thinking" => {
                if block.cache_control.is_some() {
                    dropped.push("cache_control (no Chat Completions equivalent)".into());
                }
                thinking_blocks.push(ThinkingBlock {
                    block_type: Some(block.block_type.clone()),
                    thinking: block.thinking.clone(),
                    signature: block.signature.clone(),
                    data: block.data.clone(),
                })
            }
            "tool_use" | "server_tool_use" => {
                let name = block.name.clone().unwrap_or_default();
                let upstream = sanitize_name(&name, OPENAI_NAME_MAX);
                tools.register(&name, &upstream);
                tool_calls.push(ToolCall {
                    id: Some(block.id.clone().unwrap_or_else(call_id)),
                    call_type: Some("function".into()),
                    function: Some(ToolCallFunction {
                        name: Some(upstream),
                        arguments: Some(
                            block
                                .input
                                .clone()
                                .map(|input| input.to_string())
                                .unwrap_or_else(|| "{}".into()),
                        ),
                    }),
                    ..Default::default()
                });
            }
            _ => dropped.push(block.block_type.clone()),
        }
    }

    let reasoning: String = thinking_blocks
        .iter()
        .filter(|block| block.block_type.as_deref() == Some("thinking"))
        .map(|block| block.thinking.clone().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("");

    ChatMessage {
        role: "assistant".into(),
        content: (!text.is_empty()).then_some(MessageContent::Text(text)),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        thinking_blocks: (!thinking_blocks.is_empty()).then_some(thinking_blocks),
        ..Default::default()
    }
}

fn messages_tool_choice_to_chat(
    choice: &MessagesToolChoice,
    tools: &ToolNameMap,
    dropped: &mut Vec<String>,
) -> ChatToolChoice {
    match choice.choice_type.as_str() {
        "auto" => ChatToolChoice::Mode("auto".into()),
        "any" | "required" => ChatToolChoice::Mode("required".into()),
        "none" => ChatToolChoice::Mode("none".into()),
        "tool" => ChatToolChoice::Function {
            function: crate::openai::ChatToolChoiceFunction {
                name: tools
                    .upstream_name(choice.name.as_deref().unwrap_or_default())
                    .to_string(),
            },
        },
        other => {
            dropped.push(format!("tool_choice `{other}`"));
            ChatToolChoice::Mode("auto".into())
        }
    }
}

fn thinking_to_reasoning_effort(
    thinking: Option<&ThinkingConfig>,
    dropped: &mut Vec<String>,
) -> Option<String> {
    let config = thinking?;
    match config.mode.as_str() {
        "disabled" => Some("none".into()),
        "enabled" => {
            let budget = config.budget_tokens.unwrap_or(4_096);
            Some(
                if budget >= 32_768 {
                    "xhigh"
                } else if budget >= 16_384 {
                    "high"
                } else if budget >= 8_192 {
                    "medium"
                } else if budget >= 2_048 {
                    "low"
                } else {
                    "minimal"
                }
                .into(),
            )
        }
        "adaptive" => Some("medium".into()),
        other => {
            dropped.push(format!("thinking `{other}`"));
            None
        }
    }
}

pub fn chat_response_to_messages(response: ChatResponse, tools: &ToolNameMap) -> MessagesResponse {
    let choice = response.choices.into_iter().next().unwrap_or_default();
    let message = choice.message;
    let mut blocks: Vec<Block> = Vec::new();

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
    } else if let Some(reasoning) = message.reasoning_content.as_deref() {
        let mut out = Block::typed("thinking".into());
        out.thinking = Some(reasoning.to_string());
        blocks.push(out);
    }

    match &message.content {
        Some(MessageContent::Text(text)) if !text.is_empty() => {
            blocks.push(Block::text(text.clone()))
        }
        Some(MessageContent::Parts(parts)) => {
            for part in parts.iter().filter(|part| part.part_type == "text") {
                let text = part.text.clone().unwrap_or_default();
                if !text.is_empty() {
                    blocks.push(Block::text(text));
                }
            }
        }
        _ => {}
    }

    if let Some(Value::String(refusal)) = message.extras.get("refusal") {
        blocks.push(Block::text(refusal.clone()));
    }

    for call in message.tool_calls.unwrap_or_default() {
        let function = call.function.unwrap_or_default();
        let mut block = Block::typed("tool_use".into());
        block.id = Some(call.id.unwrap_or_else(call_id));
        block.name = Some(
            tools
                .client_name(function.name.as_deref().unwrap_or_default())
                .to_string(),
        );
        block.input = Some(crate::chat_to_messages::parse_tool_arguments(
            function.arguments.as_deref(),
        ));
        blocks.push(block);
    }

    if blocks.is_empty() {
        blocks.push(Block::text(""));
    }

    MessagesResponse {
        id: if response.id.is_empty() {
            message_id()
        } else {
            response.id
        },
        object: "message".into(),
        role: "assistant".into(),
        model: if response.model.is_empty() {
            "unknown".into()
        } else {
            response.model
        },
        content: blocks,
        stop_reason: Some(stop_reason_from_finish(choice.finish_reason.as_deref())),
        stop_sequence: None,
        usage: messages_usage_from_chat(response.usage.as_ref().unwrap_or(&Default::default())),
        extras: Extras::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(body: Value) -> Converted<ChatRequest> {
        messages_request_to_chat(
            &serde_json::from_value::<MessagesRequest>(body).unwrap(),
            &Options::default(),
        )
        .unwrap()
    }

    #[test]
    fn system_becomes_a_leading_chat_message() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "system": [{"type": "text", "text": "be terse"}, {"type": "text", "text": "use rust"}],
            "messages": [{"role": "user", "content": "hi"}]
        }));

        assert_eq!(converted.request.messages[0].role, "system");
        assert_eq!(
            converted.request.messages[0].text_content(),
            "be terse\n\nuse rust".to_string()
        );
    }

    #[test]
    fn tool_use_and_tool_result_become_openai_tool_shapes() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"q": "x"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "found"}]}
                ]}
            ],
            "tools": [{"name": "lookup", "input_schema": {"type": "object"}}]
        }));

        let assistant = &converted.request.messages[0];
        let call = &assistant.tool_calls.as_ref().unwrap()[0];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(call.id.as_deref(), Some("toolu_1"));
        assert_eq!(
            call.function.as_ref().unwrap().arguments.as_deref(),
            Some(r#"{"q":"x"}"#)
        );

        let tool = &converted.request.messages[1];
        assert_eq!(tool.role, "tool");
        assert_eq!(tool.tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(tool.text_content(), "found".to_string());

        let declared = converted.request.tools.as_ref().unwrap();
        assert_eq!(declared[0].function.name, "lookup");
        assert_eq!(declared[0].tool_type, "function");
    }

    #[test]
    fn tool_choice_and_stop_sequences_map_back() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": {"type": "tool", "name": "search", "disable_parallel_tool_use": true},
            "stop_sequences": ["END"],
            "top_k": 5
        }));

        match converted.request.tool_choice.as_ref().unwrap() {
            ChatToolChoice::Function { function } => assert_eq!(function.name, "search"),
            other => panic!("unexpected tool_choice {other:?}"),
        }
        assert_eq!(converted.request.parallel_tool_calls, Some(false));
        match converted.request.stop.as_ref().unwrap() {
            OneOrMany::One(value) => assert_eq!(value, "END"),
            other => panic!("unexpected stop {other:?}"),
        }
        assert!(converted.dropped.iter().any(|d| d.starts_with("top_k")));
    }

    #[test]
    fn streaming_requests_ask_upstream_for_a_usage_chunk() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100, "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }));

        assert!(converted.request.stream);
        assert!(converted.request.wants_usage_chunk());
    }

    #[test]
    fn thinking_budget_becomes_reasoning_effort() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 20_000}
        }));

        assert_eq!(converted.request.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn image_and_document_blocks_become_openai_parts() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "document", "title": "spec", "source": {"type": "base64", "media_type": "application/pdf", "data": "AA"}}
            ]}]
        }));

        match converted.request.messages[0].content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(
                    parts[0].image_url.as_ref().unwrap().url,
                    "data:image/png;base64,QUJD"
                );
                assert_eq!(
                    parts[1].file.as_ref().unwrap()["file_data"],
                    json!("data:application/pdf;base64,AA")
                );
                assert_eq!(
                    parts[1].file.as_ref().unwrap()["filename"],
                    json!("spec.pdf")
                );
            }
            other => panic!("unexpected content {other:?}"),
        }
    }

    #[test]
    fn empty_assistant_prefill_is_kept_only_when_it_carries_content() {
        let converted = convert(json!({
            "model": "gpt-4o", "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": []}
            ]
        }));

        assert_eq!(converted.request.messages.len(), 1);
    }

    #[test]
    fn response_openai_usage_unfolds_into_anthropic_buckets() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 7, "total_tokens": 17,
                       "prompt_tokens_details": {"cached_tokens": 3, "cache_creation_input_tokens": 2}}
        }))
        .unwrap();

        let messages = chat_response_to_messages(response, &ToolNameMap::new());
        assert_eq!(messages.usage.input_tokens, 5);
        assert_eq!(messages.usage.output_tokens, 7);
        assert_eq!(messages.usage.cache_read_input_tokens, Some(3));
        assert_eq!(messages.usage.cache_creation_input_tokens, Some(2));
        assert_eq!(messages.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(messages.object, "message".to_string());
    }

    #[test]
    fn response_tool_calls_use_client_names_and_anthropic_blocks() {
        let mut tools = ToolNameMap::new();
        tools.register("get.time", "get_time");
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "chatcmpl-2", "object": "chat.completion", "created": 1, "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "call_9", "type": "function", "function": {"name": "get_time", "arguments": "{\"a\":1}"}}],
                "reasoning_content": "thinking"}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }))
        .unwrap();

        let messages = chat_response_to_messages(response, &tools);
        let blocks = &messages.content;
        assert_eq!(blocks[0].block_type, "thinking");
        assert_eq!(blocks[0].thinking.as_deref(), Some("thinking"));
        assert_eq!(blocks[1].block_type, "tool_use");
        assert_eq!(blocks[1].name.as_deref(), Some("get.time"));
        assert_eq!(blocks[1].id.as_deref(), Some("call_9"));
        assert_eq!(blocks[1].input, Some(json!({"a": 1})));
    }

    #[test]
    fn an_empty_completion_still_yields_a_valid_content_block() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "x", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant"}, "finish_reason": "stop"}]
        }))
        .unwrap();

        let messages = chat_response_to_messages(response, &ToolNameMap::new());
        assert_eq!(messages.content.len(), 1);
        assert_eq!(messages.content[0].block_type, "text");
        assert_eq!(messages.stop_reason.as_deref(), Some("end_turn"));
    }
}
