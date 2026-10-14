use serde_json::Value;

use crate::common::{dedup_notices, merge_extras, Extras};
use crate::error::ConversionError;
use crate::ids::{call_id, chat_id, created};
use crate::openai::{
    ChatMessage, ChatRequest, ChatResponse, ChatToolChoice, Choice, MessageContent, ToolCall,
    ToolCallFunction,
};
use crate::openai_responses::{
    ContentPart as ResponsesPart, InputContent, InputItem, InputPayload, Reasoning,
    ResponsesRequest, ResponsesResponse, ResponsesTool, ResponsesToolChoice, ResponsesUsage,
    TextParam,
};
use crate::tools::{sanitize_name, ToolNameMap, OPENAI_NAME_MAX};
use crate::{Converted, Options};

const RESPONSES_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
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
];

pub fn chat_request_to_responses(
    request: &ChatRequest,
    options: &Options,
) -> Result<Converted<ResponsesRequest>, ConversionError> {
    let mut tools = ToolNameMap::new();
    let mut dropped = Vec::new();

    if request.n.unwrap_or(1) > 1 {
        return Err(ConversionError::unsupported(
            "n",
            "the Responses API",
            "it returns a single response object",
        ));
    }

    let (instructions, rest) = split_leading_instructions(&request.messages);
    let input = messages_to_input(&request.messages[rest..], &mut tools, &mut dropped);
    if input.is_empty() && instructions.is_none() {
        return Err(ConversionError::EmptyConversation);
    }

    for field in [
        ("stop", request.stop.is_some()),
        (
            "presence_penalty",
            request.presence_penalty.is_some_and(|value| value != 0.0),
        ),
        (
            "frequency_penalty",
            request.frequency_penalty.is_some_and(|value| value != 0.0),
        ),
        ("seed", request.seed.is_some()),
        ("logprobs", request.logprobs == Some(true)),
        (
            "logit_bias",
            request
                .logit_bias
                .as_ref()
                .is_some_and(|bias| !bias.is_empty()),
        ),
        ("stream_options", request.stream_options.is_some()),
    ] {
        if field.1 {
            dropped.push(format!("{} (the Responses API has no equivalent)", field.0));
        }
    }

    let declared_tools = request.tools.as_ref().map(|declared| {
        declared
            .iter()
            .map(|tool| {
                let upstream = sanitize_name(&tool.function.name, OPENAI_NAME_MAX);
                tools.register(&tool.function.name, &upstream);
                ResponsesTool {
                    tool_type: "function".into(),
                    name: upstream,
                    description: tool.function.description.clone(),
                    parameters: tool.function.parameters.clone(),
                    strict: tool.function.strict,
                    extras: Extras::new(),
                }
            })
            .collect::<Vec<_>>()
    });

    let tool_choice = request
        .tool_choice
        .as_ref()
        .map(|choice| chat_tool_choice_to_responses(choice, &tools, &mut dropped));
    let text = request
        .response_format
        .as_ref()
        .and_then(|format| response_format_to_text(format, &mut dropped));
    let reasoning = request.reasoning_effort.as_ref().map(|effort| Reasoning {
        effort: Some(effort.clone()),
        summary: None,
        extras: Extras::new(),
    });
    dedup_notices(&mut dropped);

    let mut extras = Extras::new();
    if options.passthrough_extras {
        merge_extras(&mut extras, &request.extras, RESPONSES_FIELDS);
    }

    Ok(Converted {
        tools,
        dropped,
        request: ResponsesRequest {
            model: request.model.clone(),
            input: InputPayload::Items(input),
            instructions,
            max_output_tokens: request.token_limit().or(Some(options.default_max_tokens)),
            temperature: request.temperature,
            top_p: request.top_p,
            stream: request.stream,
            reasoning,
            tools: declared_tools,
            tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
            text,
            truncation: None,
            store: None,
            previous_response_id: None,
            include: None,
            metadata: None,
            user: request.user.clone(),
            prompt_cache_key: None,
            service_tier: None,
            safety_identifier: None,
            background: None,
            extras,
        },
    })
}

fn split_leading_instructions(messages: &[ChatMessage]) -> (Option<String>, usize) {
    let mut texts = Vec::new();
    let mut end = 0;
    for (index, message) in messages.iter().enumerate() {
        if !matches!(message.role.as_str(), "system" | "developer") {
            break;
        }
        let text = message.text_content();
        if !text.is_empty() {
            texts.push(text);
        }
        end = index + 1;
    }
    let joined = texts.join("\n\n");
    if joined.is_empty() {
        (None, end)
    } else {
        (Some(joined), end)
    }
}

fn messages_to_input(
    messages: &[ChatMessage],
    tools: &mut ToolNameMap,
    dropped: &mut Vec<String>,
) -> Vec<InputItem> {
    let mut items = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "user" => items.push(InputItem::message("user", parts_to_input(message, dropped))),
            "system" | "developer" => {
                let text = message.text_content();
                if !text.is_empty() {
                    items.push(InputItem::message(
                        message.role.as_str(),
                        InputContent::Parts(vec![ResponsesPart::input_text(text)]),
                    ));
                }
            }
            "assistant" => {
                if message.reasoning_content.is_some() || message.thinking_blocks.is_some() {
                    dropped.push(
                        "assistant reasoning history (Responses replays only encrypted reasoning)"
                            .to_string(),
                    );
                }
                let text = message.text_content();
                if !text.is_empty() {
                    items.push(InputItem {
                        item_type: "message".into(),
                        role: Some("assistant".into()),
                        content: Some(InputContent::Parts(vec![ResponsesPart::output_text(text)])),
                        ..Default::default()
                    });
                }
                for call in message.tool_calls.clone().unwrap_or_default() {
                    let function = call.function.unwrap_or_default();
                    let declared = function.name.unwrap_or_default();
                    let upstream = sanitize_name(&declared, OPENAI_NAME_MAX);
                    tools.register(&declared, &upstream);
                    let id = call.id.unwrap_or_else(call_id);
                    items.push(InputItem::function_call(
                        Some(format!("fc_{id}")),
                        Some(id),
                        &upstream,
                        function.arguments.unwrap_or_else(|| "{}".into()).as_str(),
                    ));
                }
            }
            "tool" | "function" => items.push(InputItem {
                item_type: "function_call_output".into(),
                call_id: message.tool_call_id.clone(),
                name: message.name.clone(),
                output: Some(Value::String(message.text_content())),
                ..Default::default()
            }),
            other => dropped.push(format!("role `{other}`")),
        }
    }
    items
}

fn parts_to_input(message: &ChatMessage, dropped: &mut Vec<String>) -> InputContent {
    match &message.content {
        None => InputContent::Text(String::new()),
        Some(MessageContent::Text(text)) => InputContent::Text(text.clone()),
        Some(MessageContent::Parts(parts)) => InputContent::Parts(
            parts
                .iter()
                .filter_map(|part| match part.part_type.as_str() {
                    "text" => Some(ResponsesPart {
                        part_type: "input_text".into(),
                        text: Some(part.text.clone().unwrap_or_default()),
                        ..Default::default()
                    }),
                    "image_url" => Some(ResponsesPart {
                        part_type: "input_image".into(),
                        image_url: part.image_url.as_ref().map(|url| url.url.clone()),
                        detail: part.image_url.as_ref().and_then(|url| url.detail.clone()),
                        ..Default::default()
                    }),
                    "file" => {
                        let file = part.file.clone().unwrap_or(Value::Null);
                        Some(ResponsesPart {
                            part_type: "input_file".into(),
                            file_url: file
                                .get("file_url")
                                .and_then(Value::as_str)
                                .map(String::from),
                            file_data: file
                                .get("file_data")
                                .and_then(Value::as_str)
                                .map(String::from),
                            file_id: file
                                .get("file_id")
                                .and_then(Value::as_str)
                                .map(String::from),
                            filename: file
                                .get("filename")
                                .and_then(Value::as_str)
                                .map(String::from),
                            ..Default::default()
                        })
                    }
                    other => {
                        dropped.push(format!("content part `{other}`"));
                        None
                    }
                })
                .collect(),
        ),
    }
}

fn chat_tool_choice_to_responses(
    choice: &ChatToolChoice,
    tools: &ToolNameMap,
    dropped: &mut Vec<String>,
) -> ResponsesToolChoice {
    match choice {
        ChatToolChoice::Mode(mode) if matches!(mode.as_str(), "auto" | "required" | "none") => {
            ResponsesToolChoice::Mode(mode.clone())
        }
        ChatToolChoice::Mode(mode) => {
            dropped.push(format!("tool_choice `{mode}`"));
            ResponsesToolChoice::Mode("auto".into())
        }
        ChatToolChoice::Function { function } => ResponsesToolChoice::Named {
            choice_type: "function".into(),
            name: Some(tools.upstream_name(&function.name).to_string()),
        },
        ChatToolChoice::Other(value) => {
            let name = value
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| value.get("name"))
                .and_then(Value::as_str);
            match name {
                Some(name) => ResponsesToolChoice::Named {
                    choice_type: "function".into(),
                    name: Some(tools.upstream_name(name).to_string()),
                },
                None => {
                    dropped.push(format!("tool_choice {value}"));
                    ResponsesToolChoice::Mode("auto".into())
                }
            }
        }
    }
}

fn response_format_to_text(
    format: &crate::openai::ResponseFormat,
    dropped: &mut Vec<String>,
) -> Option<TextParam> {
    match format.format_type.as_str() {
        "json_object" => Some(TextParam {
            format: Some(serde_json::json!({"type": "json_object"})),
            extras: Extras::new(),
        }),
        "json_schema" => Some(TextParam {
            format: format.json_schema.clone().map(|schema| {
                let mut out = Extras::new();
                out.insert("type".into(), Value::String("json_schema".into()));
                if let Some(name) = schema.get("name").and_then(Value::as_str) {
                    out.insert("name".into(), Value::String(name.to_string()));
                }
                if let Some(strict) = schema.get("strict").and_then(Value::as_bool) {
                    out.insert("strict".into(), Value::Bool(strict));
                }
                out.insert(
                    "schema".into(),
                    schema
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                );
                Value::Object(out)
            }),
            extras: Extras::new(),
        }),
        "text" => None,
        other => {
            dropped.push(format!("response_format `{other}`"));
            None
        }
    }
}

pub fn responses_response_to_chat(
    response: ResponsesResponse,
    tools: &ToolNameMap,
) -> ChatResponse {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut refusal: Option<String> = None;
    let mut tool_calls = Vec::new();

    for item in &response.output {
        match item.item_type.as_str() {
            "message" => {
                for part in item.content.clone().unwrap_or_default() {
                    match part.part_type.as_str() {
                        "output_text" | "input_text" => {
                            text.push_str(part.text.clone().unwrap_or_default().as_str())
                        }
                        "refusal" => refusal = part.refusal.clone(),
                        _ => {}
                    }
                }
            }
            "reasoning" => {
                reasoning.push_str(&summary_text(&item.summary));
            }
            "function_call" => {
                let declared = item.name.clone().unwrap_or_default();
                tool_calls.push(ToolCall {
                    id: Some(
                        item.call_id
                            .clone()
                            .or_else(|| item.id.clone())
                            .unwrap_or_else(call_id),
                    ),
                    call_type: Some("function".into()),
                    function: Some(ToolCallFunction {
                        name: Some(tools.client_name(&declared).to_string()),
                        arguments: Some(item.arguments.clone().unwrap_or_else(|| "{}".to_string())),
                    }),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    let mut extras = Extras::new();
    if let Some(refusal) = refusal {
        extras.insert("refusal".into(), Value::String(refusal));
    }

    let has_calls = !tool_calls.is_empty();
    let message = ChatMessage {
        role: "assistant".into(),
        content: (!text.is_empty()).then_some(MessageContent::Text(text)),
        tool_calls: has_calls.then_some(tool_calls),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        extras,
        ..Default::default()
    };

    let finish_reason = finish_reason_of(&response, has_calls);
    let usage = response.usage.as_ref().map(chat_usage_of);
    let id = if response.id.is_empty() {
        chat_id()
    } else {
        response.id.clone()
    };
    let created_at = if response.created_at == 0 {
        created()
    } else {
        response.created_at
    };
    let model = response.model.clone();

    ChatResponse {
        id,
        object: "chat.completion".into(),
        created: created_at,
        model,
        choices: vec![Choice {
            index: 0,
            finish_reason: Some(finish_reason),
            message,
            ..Default::default()
        }],
        usage,
        extras: Extras::new(),
    }
}

fn summary_text(summary: &Option<Vec<Value>>) -> String {
    summary
        .as_ref()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn finish_reason_of(response: &ResponsesResponse, has_calls: bool) -> String {
    if has_calls {
        return "tool_calls".into();
    }
    match response.status.as_str() {
        "incomplete" => {
            let reason = response
                .incomplete_details
                .as_ref()
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str);
            if reason == Some("content_filter") {
                "content_filter".into()
            } else {
                "length".into()
            }
        }
        "failed" | "cancelled" => "error".into(),
        _ => "stop".into(),
    }
}

/// Responses counts cached and reasoning tokens inside the totals, unlike
/// Anthropic, so the buckets only become detail fields.
fn chat_usage_of(usage: &ResponsesUsage) -> crate::openai::ChatUsage {
    let cached = usage
        .input_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens);
    let reasoning = usage
        .output_tokens_details
        .as_ref()
        .and_then(|details| details.reasoning_tokens);
    crate::openai::ChatUsage {
        prompt_tokens: usage.input_tokens,
        completion_tokens: usage.output_tokens,
        total_tokens: if usage.total_tokens > 0 {
            usage.total_tokens
        } else {
            usage.input_tokens + usage.output_tokens
        },
        prompt_tokens_details: cached.map(|cached| crate::openai::UsageDetails {
            cached_tokens: Some(cached),
            ..Default::default()
        }),
        completion_tokens_details: reasoning.map(|reasoning| crate::openai::UsageDetails {
            reasoning_tokens: Some(reasoning),
            ..Default::default()
        }),
        extras: usage.extras.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(body: Value) -> Converted<ResponsesRequest> {
        chat_request_to_responses(
            &serde_json::from_value::<ChatRequest>(body).unwrap(),
            &Options::default(),
        )
        .unwrap()
    }

    #[test]
    fn leading_system_messages_become_instructions() {
        let converted = convert(json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "developer", "content": "prefer rust"},
                {"role": "user", "content": "hi"}
            ]
        }));

        assert_eq!(
            converted.request.instructions.as_deref(),
            Some("be terse\n\nprefer rust")
        );
        assert_eq!(
            converted.request.input,
            InputPayload::Items(vec![InputItem::message(
                "user",
                InputContent::Text("hi".into()),
            )])
        );
    }

    #[test]
    fn assistant_tool_calls_split_into_function_call_items() {
        let converted = convert(json!({
            "model": "gpt-5",
            "messages": [
                {"role": "user", "content": "time?"},
                {"role": "assistant", "content": "checking", "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get.time", "arguments": "{\"city\":\"Oslo\"}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "22:00"}
            ],
            "tools": [{"type": "function", "function": {"name": "get.time", "parameters": {"type": "object"}}}]
        }));

        let InputPayload::Items(items) = &converted.request.input else {
            panic!("expected an item array");
        };
        assert_eq!(items[0].item_type, "message");
        assert_eq!(items[1].item_type, "message");
        assert_eq!(items[1].content.as_ref().unwrap().plain_text(), "checking");
        assert_eq!(items[2].item_type, "function_call");
        assert_eq!(items[2].call_id.as_deref(), Some("call_1"));
        assert_eq!(items[2].id.as_deref(), Some("fc_call_1"));
        assert_eq!(items[2].name.as_deref(), Some("get_time"));
        assert_eq!(items[3].item_type, "function_call_output");
        assert_eq!(items[3].output, Some(json!("22:00")));

        let declared = converted.request.tools.as_ref().unwrap();
        assert_eq!(declared[0].name, "get_time");
        assert_eq!(declared[0].tool_type, "function".to_string());
        assert_eq!(converted.tools.client_name("get_time"), "get.time");
    }

    #[test]
    fn chat_params_map_onto_their_responses_names() {
        let converted = convert(json!({
            "model": "gpt-5",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 128,
            "tool_choice": {"type": "function", "function": {"name": "search"}},
            "response_format": {"type": "json_schema", "json_schema": {
                "name": "plan", "schema": {"type": "object"}, "strict": true}},
            "reasoning_effort": "medium",
            "stop": ["END"],
            "user": "u-1"
        }));

        assert_eq!(converted.request.max_output_tokens, Some(128));
        assert_eq!(
            converted.request.tool_choice,
            Some(ResponsesToolChoice::Named {
                choice_type: "function".into(),
                name: Some("search".into()),
            })
        );
        assert_eq!(
            converted.request.text.unwrap().format,
            Some(
                json!({"type": "json_schema", "name": "plan", "strict": true, "schema": {"type": "object"}})
            )
        );
        assert_eq!(
            converted.request.reasoning.unwrap().effort.as_deref(),
            Some("medium")
        );
        assert_eq!(converted.request.user.as_deref(), Some("u-1"));
        assert!(converted.dropped.iter().any(|d| d.starts_with("stop")));
    }

    #[test]
    fn reasoning_history_is_dropped_with_a_notice() {
        let converted = convert(json!({
            "model": "gpt-5",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "答案", "reasoning_content": "private"}
            ]
        }));

        let InputPayload::Items(items) = &converted.request.input else {
            panic!("expected an item array");
        };
        assert_eq!(items.len(), 2);
        assert!(converted.dropped.iter().any(|d| d.contains("reasoning")));
    }

    #[test]
    fn multiple_completions_are_rejected() {
        let error = chat_request_to_responses(
            &serde_json::from_value::<ChatRequest>(json!({
                "model": "gpt-5", "n": 2, "messages": [{"role": "user", "content": "x"}]
            }))
            .unwrap(),
            &Options::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("`n`"));
    }

    #[test]
    fn response_items_fold_back_into_one_chat_choice() {
        let response: ResponsesResponse = serde_json::from_value(json!({
            "id": "resp_3", "object": "response", "created_at": 9, "status": "completed", "model": "gpt-5",
            "output": [
                {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "because"}]},
                {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                 "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get_time", "arguments": "{\"a\":1}"}
            ],
            "usage": {"input_tokens": 8, "output_tokens": 3, "total_tokens": 11,
                       "input_tokens_details": {"cached_tokens": 2},
                       "output_tokens_details": {"reasoning_tokens": 1}}
        }))
        .unwrap();

        let mut tools = ToolNameMap::new();
        tools.register("get.time", "get_time");
        let chat = responses_response_to_chat(response, &tools);

        assert_eq!(chat.id, "resp_3".to_string());
        assert_eq!(chat.created, 9);
        assert_eq!(chat.choices[0].finish_reason.as_deref(), Some("tool_calls"));
        let message = &chat.choices[0].message;
        assert_eq!(message.text_content(), "hello".to_string());
        assert_eq!(message.reasoning_content.as_deref(), Some("because"));
        let call = &message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.id.as_deref(), Some("call_1"));
        assert_eq!(
            call.function.as_ref().unwrap().name.as_deref(),
            Some("get.time")
        );

        let usage = chat.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 8);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(
            usage.prompt_tokens_details.as_ref().unwrap().cached_tokens,
            Some(2)
        );
        assert_eq!(
            usage
                .completion_tokens_details
                .as_ref()
                .unwrap()
                .reasoning_tokens,
            Some(1)
        );
    }

    #[test]
    fn incomplete_status_maps_onto_the_right_finish_reason() {
        let mut response: ResponsesResponse = serde_json::from_value(json!({
            "id": "resp_4", "object": "response", "created_at": 1, "status": "incomplete", "model": "m",
            "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                        "content": [{"type": "output_text", "text": "cut"}]}],
            "incomplete_details": {"reason": "max_output_tokens"}
        }))
        .unwrap();

        let chat = responses_response_to_chat(response.clone(), &ToolNameMap::new());
        assert_eq!(chat.choices[0].finish_reason.as_deref(), Some("length"));

        response.incomplete_details = Some(json!({"reason": "content_filter"}));
        let chat = responses_response_to_chat(response, &ToolNameMap::new());
        assert_eq!(
            chat.choices[0].finish_reason.as_deref(),
            Some("content_filter")
        );
    }

    #[test]
    fn an_empty_response_still_yields_a_valid_choice() {
        let response: ResponsesResponse = serde_json::from_value(json!({
            "id": "resp_5", "object": "response", "created_at": 1, "status": "completed",
            "model": "m", "output": []
        }))
        .unwrap();

        let chat = responses_response_to_chat(response, &ToolNameMap::new());
        assert_eq!(chat.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(chat.choices[0].message.content.is_none());
        assert_eq!(chat.usage, None);
    }
}
