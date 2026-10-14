use protocol::{
    response_events as ev, ChatChunk, ChatToResponsesStream, ResponsesEvent, ResponsesResponse,
    ResponsesToChatStream, ToolNameMap,
};
use serde_json::json;

fn event(value: serde_json::Value) -> ResponsesEvent {
    serde_json::from_value(value).unwrap()
}

fn text_chunk(id: &str, content: &str) -> ChatChunk {
    serde_json::from_value(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {"content": content}}]
    }))
    .unwrap()
}

fn describe(events: &[ResponsesEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event.event_type.as_str() {
            "response.output_item.added" | "response.output_item.done" => format!(
                "{}:{}",
                event.event_type,
                event
                    .item
                    .as_ref()
                    .map(|item| item.item_type.as_str())
                    .unwrap_or("?")
            ),
            other => other.to_string(),
        })
        .collect()
}

fn run_response(events: &[ResponsesEvent]) -> Vec<ChatChunk> {
    let mut stream = ResponsesToChatStream::new("gpt-5", ToolNameMap::new());
    let mut chunks = Vec::new();
    for event in events {
        chunks.extend(stream.push(event));
    }
    chunks.extend(stream.finish());
    chunks
}

fn contents(chunks: &[ChatChunk]) -> String {
    chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .filter_map(|choice| choice.delta.content.clone())
        .collect()
}

#[test]
fn response_events_become_chat_chunks_in_order() {
    let chunks = run_response(&[
        event(json!({"type": "response.created", "response": {
            "id": "resp_1", "object": "response", "created_at": 3, "status": "in_progress",
            "model": "gpt-5", "output": []}})),
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
        ),
        event(
            json!({"type": "response.content_part.added", "item_id": "msg_1", "output_index": 0,
            "content_index": 0, "part": {"type": "output_text", "text": ""}}),
        ),
        event(
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0,
            "content_index": 0, "delta": "Hel"}),
        ),
        event(
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0,
            "content_index": 0, "delta": "lo"}),
        ),
        event(json!({"type": "response.output_text.done", "item_id": "msg_1", "text": "Hello"})),
        event(json!({"type": "response.content_part.done", "item_id": "msg_1"})),
        event(
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant",
                      "content": [{"type": "output_text", "text": "Hello"}]}}),
        ),
        event(json!({"type": "response.completed", "response": {
            "id": "resp_1", "object": "response", "created_at": 3, "status": "completed",
            "model": "gpt-5", "output": [],
            "usage": {"input_tokens": 10, "output_tokens": 4, "total_tokens": 14,
                       "input_tokens_details": {"cached_tokens": 6}}}})),
    ]);

    assert_eq!(contents(&chunks), "Hello".to_string());
    assert_eq!(
        chunks[0].choices[0].delta.role.as_deref(),
        Some("assistant")
    );
    assert_eq!(
        chunks[0].id,
        "resp_1".to_string(),
        "the response id is kept"
    );
    assert_eq!(chunks[0].created, 3);

    let closing = chunks
        .iter()
        .position(|chunk| chunk.choices.iter().any(|c| c.finish_reason.is_some()))
        .expect("a finish frame");
    assert_eq!(
        chunks[closing].choices[0].finish_reason.as_deref(),
        Some("stop")
    );
    let usage = chunks[closing + 1].usage.clone().expect("a usage frame");
    assert_eq!(usage.prompt_tokens, 10, "cached tokens are not added twice");
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.total_tokens, 14);
    assert_eq!(usage.prompt_tokens_details.unwrap().cached_tokens, Some(6));
}

#[test]
fn function_call_items_become_tool_call_deltas() {
    let mut tools = ToolNameMap::new();
    tools.register("get.time", "get_time");
    let mut stream = ResponsesToChatStream::new("gpt-5", tools);
    let mut chunks = Vec::new();
    for item in [
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get_time", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
               "output_index": 0, "delta": "{\"a\""}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
               "output_index": 0, "delta": ":1}"}),
        json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "arguments": "{\"a\":1}"}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "get_time", "arguments": "{\"a\":1}"}}),
        json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "model": "m", "output": [],
               "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}}),
    ] {
        chunks.extend(stream.push(&event(item)));
    }

    let arguments: String = chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .flat_map(|choice| choice.delta.tool_calls.clone().unwrap_or_default())
        .filter_map(|call| call.function.and_then(|f| f.arguments))
        .collect();
    assert_eq!(arguments, r#"{"a":1}"#.to_string());

    let opened = &chunks[0].choices[0].delta.tool_calls.as_ref().unwrap()[0];
    assert_eq!(
        opened.id.as_deref(),
        Some("call_1"),
        "call_id is the Chat tool call id"
    );
    assert_eq!(
        opened.function.as_ref().unwrap().name.as_deref(),
        Some("get.time")
    );
    assert_eq!(opened.index, Some(0));

    let finish = chunks
        .iter()
        .find(|chunk| chunk.choices.iter().any(|c| c.finish_reason.is_some()))
        .expect("a finish frame");
    assert_eq!(
        finish.choices[0].finish_reason.as_deref(),
        Some("tool_calls")
    );
}

#[test]
fn a_tool_with_no_argument_deltas_still_reports_an_empty_object() {
    let chunks = run_response(&[
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "ping", "arguments": ""}}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "ping", "arguments": "{}"}}),
        ),
    ]);

    let closing = chunks
        .iter()
        .rev()
        .find(|chunk| chunk.choices.iter().any(|c| c.delta.tool_calls.is_some()))
        .expect("a tool call frame");
    let call = &closing.choices[0].delta.tool_calls.as_ref().unwrap()[0];
    assert_eq!(
        call.function.as_ref().unwrap().arguments.as_deref(),
        Some("{}")
    );
}

#[test]
fn reasoning_summary_deltas_map_onto_reasoning_content() {
    let chunks = run_response(&[
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        ),
        event(
            json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1",
            "output_index": 0, "summary_index": 0, "delta": "think"}),
        ),
    ]);

    let reasoning: String = chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .filter_map(|choice| choice.delta.reasoning_content.clone())
        .collect();
    assert_eq!(reasoning, "think".to_string());
}

#[test]
fn stream_errors_and_aborts_still_close_the_response() {
    let chunks = run_response(&[event(json!({
        "type": "error", "code": "rate_limit_exceeded", "message": "slow down"
    }))]);
    let error = chunks[0].extras.get("error").expect("an error payload");
    assert_eq!(error["message"], json!("slow down"));
    assert_eq!(error["type"], json!("rate_limit_exceeded"));

    let chunks = run_response(&[
        event(json!({"type": "response.created", "response": {
            "id": "resp_2", "object": "response", "created_at": 1, "status": "in_progress",
            "model": "gpt-5", "output": []}})),
        event(
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
            "output_index": 0, "content_index": 0, "delta": "partial"}),
        ),
    ]);
    let finish = chunks
        .iter()
        .find(|chunk| chunk.choices.iter().any(|c| c.finish_reason.is_some()))
        .expect("a finish frame even when the upstream never sent response.completed");
    assert_eq!(finish.choices[0].finish_reason.as_deref(), Some("stop"));
}

#[test]
fn failed_response_reports_the_upstream_error() {
    let chunks = run_response(&[event(json!({
        "type": "response.failed",
        "response": {"id": "r", "status": "failed", "model": "m", "output": [],
                     "error": {"code": "server_error", "message": "boom"}}
    }))]);

    let error = chunks[0].extras.get("error").expect("an error payload");
    assert_eq!(error["message"], json!("boom"));
}

fn run_chat(chunks: &[ChatChunk]) -> (Vec<ResponsesEvent>, Vec<ResponsesEvent>) {
    let mut stream = ChatToResponsesStream::new("gpt-5", ToolNameMap::new());
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(stream.push(chunk));
    }
    let tail = stream.finish();
    (events, tail)
}

#[test]
fn chat_chunks_become_the_full_response_lifecycle() {
    let usage = serde_json::from_value::<ChatChunk>(json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "gpt-5",
        "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    }))
    .unwrap();
    let finish = serde_json::from_value::<ChatChunk>(json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    }))
    .unwrap();

    let opening = text_chunk("chatcmpl-1", "Hel");
    let opening = ChatChunk {
        created: 7,
        ..opening
    };
    let (events, tail) = run_chat(&[opening, text_chunk("chatcmpl-1", "lo"), finish, usage]);
    assert!(tail.is_empty());

    assert_eq!(
        describe(&events),
        vec![
            ev::CREATED,
            ev::IN_PROGRESS,
            "response.output_item.added:message",
            ev::CONTENT_PART_ADDED,
            ev::OUTPUT_TEXT_DELTA,
            ev::OUTPUT_TEXT_DELTA,
            ev::OUTPUT_TEXT_DONE,
            ev::CONTENT_PART_DONE,
            "response.output_item.done:message",
            ev::COMPLETED,
        ]
    );

    let numbers: Vec<u64> = events
        .iter()
        .map(|event| event.sequence_number.unwrap_or_default())
        .collect();
    assert_eq!(numbers, (1..=numbers.len() as u64).collect::<Vec<_>>());

    match &events[0].response {
        Some(response) => {
            assert_eq!(response.id, "chatcmpl-1".to_string());
            assert_eq!(response.status, "in_progress".to_string());
            assert_eq!(response.created_at, 7);
        }
        None => panic!("response.created carries the response"),
    }

    let completed = events
        .last()
        .and_then(|event| event.response.as_ref())
        .unwrap();
    assert_eq!(completed.status, "completed".to_string());
    assert_eq!(completed.output[0].item_type, "message");
    assert_eq!(completed.output[0].text_parts(), "Hello".to_string());
    assert_eq!(completed.usage.as_ref().unwrap().input_tokens, 5);
    assert_eq!(completed.usage.as_ref().unwrap().output_tokens, 2);
}

#[test]
fn text_then_tools_close_the_message_item_first() {
    let tool = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "type": "function", "function": {"name": "get_time", "arguments": "{\"a\""}}]}}]
    }))
    .unwrap();
    let more = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": ":1}"}}]}}]
    }))
    .unwrap();
    let finish = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
    }))
    .unwrap();

    let (events, _) = run_chat(&[text_chunk("c", "hi"), tool, more, finish]);

    assert_eq!(
        describe(&events)[2..],
        [
            "response.output_item.added:message",
            ev::CONTENT_PART_ADDED,
            ev::OUTPUT_TEXT_DELTA,
            ev::OUTPUT_TEXT_DONE,
            ev::CONTENT_PART_DONE,
            "response.output_item.done:message",
            "response.output_item.added:function_call",
            ev::FUNCTION_CALL_ARGS_DELTA,
            ev::FUNCTION_CALL_ARGS_DELTA,
            ev::FUNCTION_CALL_ARGS_DONE,
            "response.output_item.done:function_call",
            ev::COMPLETED,
        ]
    );

    let completed = events.last().unwrap().response.clone().unwrap();
    assert_eq!(completed.output.len(), 2);
    assert_eq!(completed.output[1].item_type, "function_call");
    assert_eq!(completed.output[1].call_id.as_deref(), Some("call_1"));
    assert_eq!(completed.output[1].id.as_deref(), Some("fc_call_1"));
    assert_eq!(
        completed.output[1].arguments.as_deref(),
        Some(r#"{"a":1}"#),
        "argument fragments are accumulated for the done event"
    );
}

#[test]
fn a_truncated_completion_ends_as_incomplete() {
    let finish = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {"content": "cut"}, "finish_reason": "length"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 9, "total_tokens": 10}
    }))
    .unwrap();

    let (events, _) = run_chat(&[finish]);
    let last = events.last().unwrap();
    assert_eq!(last.event_type, ev::INCOMPLETE);
    let response = last.response.clone().unwrap();
    assert_eq!(response.status, "incomplete".to_string());
    assert_eq!(
        response.incomplete_details.unwrap()["reason"],
        json!("max_output_tokens")
    );
}

#[test]
fn reasoning_opens_a_reasoning_item_with_summary_parts() {
    let chunk = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {"reasoning_content": "think"}}]
    }))
    .unwrap();
    let finish = serde_json::from_value::<ChatChunk>(json!({
        "id": "c", "object": "chat.completion.chunk", "created": 1, "model": "gpt-5",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    }))
    .unwrap();

    let (events, _) = run_chat(&[chunk, finish]);
    assert_eq!(
        describe(&events),
        vec![
            ev::CREATED,
            ev::IN_PROGRESS,
            "response.output_item.added:reasoning",
            ev::REASONING_SUMMARY_PART_ADDED,
            ev::REASONING_SUMMARY_TEXT_DELTA,
            ev::REASONING_SUMMARY_TEXT_DONE,
            ev::REASONING_SUMMARY_PART_DONE,
            "response.output_item.done:reasoning",
            ev::COMPLETED,
        ]
    );
}

#[test]
fn an_upstream_failure_emits_response_failed_and_stops() {
    let mut stream = ChatToResponsesStream::new("gpt-5", ToolNameMap::new());
    let first = stream.push(&text_chunk("c", "partial"));
    let failure = stream.failure("upstream exploded");

    assert_eq!(describe(&first)[0], ev::CREATED);
    assert_eq!(failure.last().unwrap().event_type, ev::FAILED);
    let response = failure.last().unwrap().response.clone().unwrap();
    assert_eq!(response.status, "failed".to_string());
    assert_eq!(
        response.error.unwrap()["message"],
        json!("upstream exploded")
    );
    assert!(stream.push(&text_chunk("c", "late")).is_empty());
    assert!(stream.finish().is_empty());
}

#[test]
fn an_empty_upstream_stream_still_produces_a_valid_response() {
    let (events, tail) = run_chat(&[]);
    assert!(events.is_empty());
    assert_eq!(
        describe(&tail),
        vec![ev::CREATED, ev::IN_PROGRESS, ev::COMPLETED]
    );
    let completed: &ResponsesResponse = tail.last().unwrap().response.as_ref().unwrap();
    assert_eq!(completed.status, "completed".to_string());
    assert!(completed.output.is_empty());
}
