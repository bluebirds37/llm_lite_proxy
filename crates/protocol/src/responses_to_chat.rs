use serde_json::Value;

use crate::common::{dedup_notices, Extras};
use crate::error::ConversionError;
use crate::ids::call_id;
use crate::openai::{
    ChatMessage, ChatRequest, ChatResponse, ChatTool, ChatToolChoice, ChatToolFunction,
    ContentPart, MessageContent, ResponseFormat, StreamOptions, ToolCall, ToolCallFunction,
};
use crate::openai_responses::{
    ContentPart as ResponsesPart, InputContent, InputItem, InputPayload, ResponsesRequest,
    ResponsesResponse, ResponsesToolChoice, ResponsesUsage, TextParam, UsageDetails,
};
use crate::tools::{sanitize_name, ToolNameMap, OPENAI_NAME_MAX};
use crate::{Converted, Options};

const CHAT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "instructions",
    "input",
    "max_output_tokens",
    "temperature",
    "top_p",
    "stream",
    "reasoning",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "text",
    "user",
    "max_tokens",
    "max_completion_tokens",
    "response_format",
    "reasoning_effort",
    "store",
    "truncation",
    "metadata",
    "previous_response_id",
    "include",
    "prompt_cache_key",
    "service_tier",
    "safety_identifier",
    "background",
];

/// A Responses request folded into the canonical Chat Completions shape.
pub fn responses_request_to_chat(
    request: &ResponsesRequest,
    options: &Options,
) -> Result<Converted<ChatRequest>, ConversionError> {
    let mut tools = ToolNameMap::new();
    let mut dropped = Vec::new();
    let mut messages: Vec<ChatMessage> = Vec::new();

    if request.previous_response_id.is_some() {
        return Err(ConversionError::unsupported(
            "previous_response_id",
            "Chat Completions",
            "the proxy keeps no stored response to continue from",
        ));
    }
    if request.background == Some(true) {
        return Err(ConversionError::unsupported(
            "background",
            "Chat Completions",
            "only synchronous completions are proxied",
        ));
    }
    if request
        .include
        .as_ref()
        .is_some_and(|items| !items.is_empty())
    {
        return Err(ConversionError::unsupported(
            "include",
            "Chat Completions",
            "it asks for server-side data this endpoint never returns",
        ));
    }
    if request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.summary.as_deref())
        .is_some()
    {
        dropped.push("reasoning.summary (Chat Completions has no summary control)".into());
    }

    if let Some(instructions) = request
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        messages.push(ChatMessage::text("system", instructions));
    }

    match &request.input {
        InputPayload::Text(text) => messages.push(ChatMessage::text("user", text.clone())),
        InputPayload::Items(items) => {
            for item in items {
                if let Some(message) = input_item_to_chat(item, &mut tools, &mut dropped)? {
                    messages.push(message);
                }
            }
        }
    }

    if messages.is_empty() {
        return Err(ConversionError::EmptyConversation);
    }

    for field in [
        "store",
        "truncation",
        "metadata",
        "prompt_cache_key",
        "service_tier",
        "safety_identifier",
    ] {
        if request.extras.contains_key(field) || requested_field_present(request, field) {
            dropped.push(format!("{field} (Chat Completions has no equivalent)"));
        }
    }
    dedup_notices(&mut dropped);

    let declared_tools = request.tools.as_ref().map(|declared| {
        declared
            .iter()
            .map(|tool| {
                let upstream = sanitize_name(&tool.name, OPENAI_NAME_MAX);
                tools.register(&tool.name, &upstream);
                ChatTool {
                    tool_type: "function".into(),
                    function: ChatToolFunction {
                        name: upstream,
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                        strict: tool.strict,
                        extras: Extras::new(),
                    },
                    extras: tool.extras.clone(),
                }
            })
            .collect::<Vec<_>>()
    });

    let tool_choice = request
        .tool_choice
        .as_ref()
        .map(|choice| responses_tool_choice_to_chat(choice, &mut dropped));
    let response_format = request
        .text
        .as_ref()
        .and_then(text_format_to_response_format);
    dedup_notices(&mut dropped);

    Ok(Converted {
        tools,
        dropped,
        request: ChatRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_output_tokens,
            temperature: request.temperature,
            top_p: request.top_p,
            stream: request.stream,
            stream_options: (request.stream && options.openai_include_usage).then_some(
                StreamOptions {
                    include_usage: Some(true),
                    extras: Extras::new(),
                },
            ),
            tools: declared_tools,
            tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
            reasoning_effort: request
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.clone()),
            user: request.user.clone(),
            response_format,
            n: None,
            max_completion_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            stop: None,
            seed: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            extras: filtered_extras(request, options),
        },
    })
}

fn requested_field_present(request: &ResponsesRequest, field: &str) -> bool {
    match field {
        "store" => request.store.is_some(),
        "truncation" => request.truncation.is_some(),
        "metadata" => request.metadata.is_some(),
        "prompt_cache_key" => request.prompt_cache_key.is_some(),
        "service_tier" => request.service_tier.is_some(),
        "safety_identifier" => request.safety_identifier.is_some(),
        _ => false,
    }
}

fn filtered_extras(request: &ResponsesRequest, options: &Options) -> Extras {
    if !options.passthrough_extras {
        return Extras::new();
    }
    let mut out = Extras::new();
    for (key, value) in &request.extras {
        if !CHAT_FIELDS.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

fn input_item_to_chat(
    item: &InputItem,
    tools: &mut ToolNameMap,
    dropped: &mut Vec<String>,
) -> Result<Option<ChatMessage>, ConversionError> {
    match item.item_type.as_str() {
        "" | "message" => Ok(Some(ChatMessage {
            role: item.role.clone().unwrap_or_else(|| "user".into()),
            content: item.content.as_ref().map(content_to_message_content),
            extras: Extras::new(),
            ..Default::default()
        })),
        "function_call" => {
            let declared = item.name.clone().unwrap_or_default();
            let upstream = sanitize_name(&declared, OPENAI_NAME_MAX);
            tools.register(&declared, &upstream);
            Ok(Some(ChatMessage {
                role: "assistant".into(),
                tool_calls: Some(vec![ToolCall {
                    id: Some(
                        item.call_id
                            .clone()
                            .or_else(|| item.id.clone())
                            .unwrap_or_else(call_id),
                    ),
                    call_type: Some("function".into()),
                    function: Some(ToolCallFunction {
                        name: Some(upstream),
                        arguments: Some(item.arguments.clone().unwrap_or_else(|| "{}".into())),
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }))
        }
        "function_call_output" => Ok(Some(ChatMessage {
            role: "tool".into(),
            tool_call_id: item.call_id.clone().or_else(|| item.id.clone()),
            content: Some(MessageContent::Text(output_to_text(&item.output))),
            ..Default::default()
        })),
        "reasoning" => {
            dropped
                .push("reasoning input item (Chat Completions replays no thinking state)".into());
            Ok(None)
        }
        "item_reference" => Err(ConversionError::unsupported(
            "input[].type item_reference",
            "Chat Completions",
            "the proxy is stateless, so items can only be replayed inline",
        )),
        other => {
            dropped.push(format!("input item `{other}`"));
            Ok(None)
        }
    }
}

fn content_to_message_content(content: &InputContent) -> MessageContent {
    match content {
        InputContent::Text(text) => MessageContent::Text(text.clone()),
        InputContent::Parts(parts) => MessageContent::Parts(
            parts
                .iter()
                .map(|part| match part.part_type.as_str() {
                    "input_image" => ContentPart {
                        part_type: "image_url".into(),
                        text: None,
                        image_url: Some(crate::openai::ImageUrl {
                            url: part.image_url.clone().unwrap_or_default(),
                            detail: part.detail.clone(),
                            extras: Extras::new(),
                        }),
                        refusal: None,
                        input_audio: None,
                        file: None,
                        cache_control: None,
                        extras: part.extras.clone(),
                    },
                    "input_file" => ContentPart {
                        part_type: "file".into(),
                        text: None,
                        image_url: None,
                        refusal: None,
                        input_audio: None,
                        file: Some(file_value(part)),
                        cache_control: None,
                        extras: part.extras.clone(),
                    },
                    _ => ContentPart {
                        part_type: "text".into(),
                        text: Some(
                            part.text
                                .clone()
                                .or_else(|| part.refusal.clone())
                                .unwrap_or_default(),
                        ),
                        image_url: None,
                        refusal: None,
                        input_audio: None,
                        file: None,
                        cache_control: None,
                        extras: part.extras.clone(),
                    },
                })
                .collect(),
        ),
    }
}

fn file_value(part: &ResponsesPart) -> Value {
    let mut file = Extras::new();
    if let Some(data) = &part.file_data {
        file.insert("file_data".into(), Value::String(data.clone()));
    }
    if let Some(url) = &part.file_url {
        file.insert("file_url".into(), Value::String(url.clone()));
    }
    if let Some(id) = &part.file_id {
        file.insert("file_id".into(), Value::String(id.clone()));
    }
    if let Some(name) = &part.filename {
        file.insert("filename".into(), Value::String(name.clone()));
    }
    Value::Object(file)
}

fn output_to_text(output: &Option<Value>) -> String {
    match output {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
                    .map(String::from)
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.to_string(),
    }
}

fn responses_tool_choice_to_chat(
    choice: &ResponsesToolChoice,
    dropped: &mut Vec<String>,
) -> ChatToolChoice {
    match choice {
        ResponsesToolChoice::Mode(mode) => match mode.as_str() {
            "auto" | "required" | "none" => ChatToolChoice::Mode(mode.clone()),
            other => {
                dropped.push(format!("tool_choice `{other}`"));
                ChatToolChoice::Mode("auto".into())
            }
        },
        ResponsesToolChoice::Named { choice_type, name } => {
            if choice_type == "function" || choice_type.is_empty() {
                ChatToolChoice::Function {
                    function: crate::openai::ChatToolChoiceFunction {
                        name: name.clone().unwrap_or_default(),
                    },
                }
            } else {
                dropped.push(format!("tool_choice `{choice_type}`"));
                ChatToolChoice::Mode("auto".into())
            }
        }
        ResponsesToolChoice::Other(value) => {
            let name = value
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| value.get("name"))
                .and_then(Value::as_str);
            match name {
                Some(name) => ChatToolChoice::Function {
                    function: crate::openai::ChatToolChoiceFunction {
                        name: name.to_string(),
                    },
                },
                None => {
                    dropped.push(format!("tool_choice {value}"));
                    ChatToolChoice::Mode("auto".into())
                }
            }
        }
    }
}

fn text_format_to_response_format(text: &TextParam) -> Option<ResponseFormat> {
    let format = text.format.as_ref()?;
    let kind = format.get("type").and_then(Value::as_str).unwrap_or("text");
    match kind {
        "json_schema" => Some(ResponseFormat {
            format_type: "json_schema".into(),
            json_schema: Some(format.clone()),
            extras: Extras::new(),
        }),
        "json_object" => Some(ResponseFormat {
            format_type: "json_object".into(),
            json_schema: None,
            extras: Extras::new(),
        }),
        _ => None,
    }
}

pub fn chat_response_to_responses(
    response: ChatResponse,
    tools: &ToolNameMap,
) -> ResponsesResponse {
    let choice = response.choices.into_iter().next().unwrap_or_default();
    let message = choice.message;
    let mut output = Vec::new();
    let mut text = String::new();

    let reasoning: String = match &message.thinking_blocks {
        Some(blocks) => blocks
            .iter()
            .map(|block| block.thinking.clone().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(""),
        None => message.reasoning_content.clone().unwrap_or_default(),
    };
    if !reasoning.trim().is_empty() {
        output.push(crate::openai_responses::OutputItem {
            item_type: "reasoning".into(),
            id: Some(format!("rs_{}", crate::ids::token(20))),
            summary: Some(vec![Value::Object({
                let mut part = Extras::new();
                part.insert("type".into(), Value::String("summary_text".into()));
                part.insert("text".into(), Value::String(reasoning));
                part
            })]),
            ..Default::default()
        });
    }

    match &message.content {
        Some(MessageContent::Text(body)) => text.push_str(body),
        Some(MessageContent::Parts(parts)) => {
            for part in parts.iter().filter(|part| part.part_type == "text") {
                text.push_str(part.text.clone().unwrap_or_default().as_str());
            }
        }
        None => {}
    }
    if let Some(Value::String(refusal)) = message.extras.get("refusal") {
        text.push_str(refusal);
    }
    if !text.is_empty() || message.tool_calls.is_none() {
        output.push(crate::openai_responses::OutputItem {
            item_type: "message".into(),
            id: Some(format!("msg_{}", crate::ids::token(20))),
            role: Some("assistant".into()),
            status: Some("completed".into()),
            content: Some(vec![ResponsesPart::output_text(text.clone())]),
            ..Default::default()
        });
    }

    for call in message.tool_calls.unwrap_or_default() {
        let function = call.function.unwrap_or_default();
        let client_name = tools.client_name(function.name.as_deref().unwrap_or_default());
        let id = call.id.clone().unwrap_or_else(call_id);
        output.push(crate::openai_responses::OutputItem {
            item_type: "function_call".into(),
            call_id: Some(id.clone()),
            id: Some(format!("fc_{id}")),
            name: Some(client_name.to_string()),
            arguments: Some(function.arguments.unwrap_or_else(|| "{}".into())),
            status: Some("completed".into()),
            ..Default::default()
        });
    }

    ResponsesResponse {
        id: if response.id.is_empty() {
            format!("resp_{}", crate::ids::token(20))
        } else {
            response.id
        },
        object: "response".into(),
        created_at: response.created,
        status: status_from_finish(choice.finish_reason.as_deref()).into(),
        incomplete_details: (choice.finish_reason.as_deref() == Some("length")).then(|| {
            Value::Object({
                let mut reason = Extras::new();
                reason.insert("reason".into(), Value::String("max_output_tokens".into()));
                reason
            })
        }),
        model: response.model,
        output,
        usage: response.usage.as_ref().map(|usage| ResponsesUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            input_tokens_details: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens)
                .map(|cached| UsageDetails {
                    cached_tokens: Some(cached),
                    ..Default::default()
                }),
            output_tokens_details: usage
                .completion_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens)
                .map(|reasoning| UsageDetails {
                    reasoning_tokens: Some(reasoning),
                    ..Default::default()
                }),
            extras: Extras::new(),
        }),
        error: None,
        instructions: None,
        completed_at: None,
        sequence_number: None,
        extras: Extras::new(),
    }
}

/// Chat Completions answers carry no Responses status, so the finish reason is
/// the only signal for whether the output was cut short.
pub fn status_from_finish(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "incomplete",
        Some("content_filter") => "incomplete",
        _ => "completed",
    }
}

pub fn finish_reason_from_status(status: Option<&str>) -> String {
    match status {
        Some("incomplete") | Some("max_output_tokens") => "length".into(),
        Some("failed") | Some("cancelled") => "error".into(),
        _ => "stop".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(body: Value) -> Converted<ChatRequest> {
        responses_request_to_chat(
            &serde_json::from_value::<ResponsesRequest>(body).unwrap(),
            &Options::default(),
        )
        .unwrap()
    }

    #[test]
    fn instructions_and_input_text_become_a_system_and_user_pair() {
        let converted = convert(json!({
            "model": "gpt-5",
            "instructions": "be terse",
            "input": "hi",
            "max_output_tokens": 64
        }));

        assert_eq!(converted.request.messages[0].role, "system");
        assert_eq!(converted.request.messages[0].text_content(), "be terse");
        assert_eq!(converted.request.messages[1].role, "user");
        assert_eq!(converted.request.max_tokens, Some(64));
    }

    #[test]
    fn function_call_and_output_items_become_tool_calls_and_tool_messages() {
        let converted = convert(json!({
            "model": "gpt-5",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "time?"}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get.time", "arguments": "{\"city\":\"Oslo\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "22:00"}
            ],
            "tools": [{"type": "function", "name": "get.time", "parameters": {"type": "object"}}]
        }));

        let assistant = &converted.request.messages[1];
        assert_eq!(assistant.role, "assistant");
        let call = &assistant.tool_calls.as_ref().unwrap()[0];
        assert_eq!(
            call.id.as_deref(),
            Some("call_1"),
            "call_id is the Chat tool_call id"
        );
        assert_eq!(
            call.function.as_ref().unwrap().name.as_deref(),
            Some("get_time"),
            "history names are sanitised for the upstream too"
        );
        assert_eq!(converted.tools.client_name("get_time"), "get.time");

        let tool = &converted.request.messages[2];
        assert_eq!(tool.role, "tool");
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(tool.text_content(), "22:00".to_string());

        let declared = converted.request.tools.as_ref().unwrap();
        assert_eq!(declared[0].function.name, "get_time");
        assert_eq!(declared[0].tool_type, "function");
        assert_eq!(
            declared[0].function.parameters,
            Some(json!({"type": "object"}))
        );
    }

    #[test]
    fn image_and_file_parts_map_onto_chat_content_parts() {
        let converted = convert(json!({
            "model": "gpt-5",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": "https://x/y.png", "detail": "low"},
                {"type": "input_file", "file_data": "data:application/pdf;base64,AA", "filename": "a.pdf"}
            ]}]
        }));

        match converted.request.messages[0].content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[1].part_type, "image_url");
                assert_eq!(parts[1].image_url.as_ref().unwrap().url, "https://x/y.png");
                assert_eq!(
                    parts[1].image_url.as_ref().unwrap().detail.as_deref(),
                    Some("low")
                );
                assert_eq!(parts[2].part_type, "file");
                assert_eq!(parts[2].file.as_ref().unwrap()["filename"], json!("a.pdf"));
            }
            other => panic!("unexpected content {other:?}"),
        }
    }

    #[test]
    fn text_format_and_reasoning_effort_map_onto_chat_params() {
        let converted = convert(json!({
            "model": "gpt-5",
            "input": "hi",
            "text": {"format": {"type": "json_schema", "name": "plan", "schema": {"type": "object"}, "strict": true}},
            "reasoning": {"effort": "high", "summary": "auto"},
            "tool_choice": {"type": "function", "name": "search"},
            "user": "u-1"
        }));

        let format = converted.request.response_format.unwrap();
        assert_eq!(format.format_type, "json_schema");
        assert_eq!(format.json_schema.unwrap()["name"], json!("plan"));
        assert_eq!(converted.request.reasoning_effort.as_deref(), Some("high"));
        match converted.request.tool_choice.unwrap() {
            ChatToolChoice::Function { function } => assert_eq!(function.name, "search"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(converted.request.user.as_deref(), Some("u-1"));
    }

    #[test]
    fn stateful_responses_params_are_rejected_not_silently_dropped() {
        let error = responses_request_to_chat(
            &serde_json::from_value::<ResponsesRequest>(json!({
                "model": "gpt-5", "input": "hi", "previous_response_id": "resp_1"
            }))
            .unwrap(),
            &Options::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("previous_response_id"));

        let background = responses_request_to_chat(
            &serde_json::from_value::<ResponsesRequest>(json!({
                "model": "gpt-5", "input": "hi", "background": true
            }))
            .unwrap(),
            &Options::default(),
        )
        .unwrap_err();
        assert!(background.to_string().contains("background"));
    }

    #[test]
    fn reasoning_input_items_are_dropped_with_a_notice() {
        let converted = convert(json!({
            "model": "gpt-5",
            "input": [
                {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "hm"}], "encrypted_content": "AbC"},
                {"type": "message", "role": "user", "content": "hi"}
            ]
        }));

        assert_eq!(converted.request.messages.len(), 1);
        assert!(converted.dropped.iter().any(|d| d.starts_with("reasoning")));
    }

    #[test]
    fn chat_response_becomes_ordered_response_items() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "resp_7", "object": "chat.completion", "created": 5, "model": "gpt-5",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello",
                "reasoning_content": "because",
                "tool_calls": [{"id": "call_9", "type": "function", "function": {"name": "get_time", "arguments": "{\"a\":1}"}}]},
                "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11,
                       "prompt_tokens_details": {"cached_tokens": 2},
                       "completion_tokens_details": {"reasoning_tokens": 1}}
        }))
        .unwrap();

        let mut tools = ToolNameMap::new();
        tools.register("get.time", "get_time");
        let converted = chat_response_to_responses(response, &tools);

        assert_eq!(converted.id, "resp_7".to_string());
        assert_eq!(converted.object, "response".to_string());
        assert_eq!(converted.status, "completed".to_string());
        assert_eq!(converted.output[0].item_type, "reasoning");
        assert_eq!(converted.output[1].item_type, "message");
        assert_eq!(
            converted.output[1].content.as_ref().unwrap()[0].part_type,
            "output_text"
        );
        assert_eq!(converted.output[2].item_type, "function_call");
        assert_eq!(converted.output[2].name.as_deref(), Some("get.time"));
        assert_eq!(converted.output[2].call_id.as_deref(), Some("call_9"));
        assert_eq!(converted.output[1].text_parts(), "hello".to_string());
        assert_eq!(
            converted
                .usage
                .as_ref()
                .unwrap()
                .input_tokens_details
                .as_ref()
                .unwrap()
                .cached_tokens,
            Some(2)
        );
        assert_eq!(
            converted
                .usage
                .as_ref()
                .unwrap()
                .output_tokens_details
                .as_ref()
                .unwrap()
                .reasoning_tokens,
            Some(1)
        );
    }

    #[test]
    fn a_truncated_completion_is_reported_as_incomplete() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "x", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "cut"}, "finish_reason": "length"}]
        }))
        .unwrap();

        let converted = chat_response_to_responses(response, &ToolNameMap::new());
        assert_eq!(converted.status, "incomplete".to_string());
        assert_eq!(converted.output.len(), 1);
    }
}
