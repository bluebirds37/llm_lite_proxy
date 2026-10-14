use protocol::{
    ChatChunk, ChatToAnthropicStream, ChatUsage, ChunkChoice, Delta, StreamEvent, ThinkingBlock,
    ToolCall, ToolCallFunction, ToolNameMap, UsageDetails,
};

fn chunk(delta: Delta, finish: Option<&str>, usage: Option<ChatUsage>) -> ChatChunk {
    let choices = if finish.is_none() && usage.is_some() {
        Vec::new()
    } else {
        vec![ChunkChoice {
            index: 0,
            delta,
            finish_reason: finish.map(String::from),
        }]
    };
    ChatChunk {
        id: "chatcmpl-1".into(),
        object: "chat.completion.chunk".into(),
        created: 1,
        model: "gpt-4o".into(),
        choices,
        usage,
        extras: Default::default(),
    }
}

fn usage(prompt: u64, completion: u64, cached: u64) -> ChatUsage {
    ChatUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        prompt_tokens_details: Some(UsageDetails {
            cached_tokens: Some(cached),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn describe(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            StreamEvent::MessageStart { .. } => "message_start".to_string(),
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => format!("start:{index}:{}", content_block.block_type),
            StreamEvent::ContentBlockDelta { index, delta } => {
                format!("delta:{index}:{}", delta.delta_type)
            }
            StreamEvent::ContentBlockStop { index } => format!("stop:{index}"),
            StreamEvent::MessageDelta { delta, .. } => format!(
                "message_delta:{}",
                delta.stop_reason.as_deref().unwrap_or("-")
            ),
            StreamEvent::MessageStop => "message_stop".to_string(),
            StreamEvent::Ping => "ping".to_string(),
            StreamEvent::Error { .. } => "error".to_string(),
        })
        .collect()
}

fn run(chunks: &[ChatChunk]) -> (Vec<StreamEvent>, Vec<StreamEvent>) {
    let mut stream = ChatToAnthropicStream::new("gpt-4o", ToolNameMap::new());
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(stream.push(chunk));
    }
    let tail = stream.finish();
    (events, tail)
}

fn content(text: &str) -> Delta {
    Delta {
        content: Some(text.to_string()),
        ..Default::default()
    }
}

#[test]
fn text_then_tools_produce_a_closed_block_ladder() {
    let role = Delta {
        role: Some("assistant".into()),
        content: Some(String::new()),
        ..Default::default()
    };
    let head_tool = chunk(
        Delta {
            tool_calls: Some(vec![ToolCall {
                index: Some(0),
                id: Some("call_1".into()),
                function: Some(ToolCallFunction {
                    name: Some("run".into()),
                    arguments: Some("{\"a".into()),
                }),
                ..Default::default()
            }]),
            ..Default::default()
        },
        None,
        None,
    );
    let tail_tool = chunk(
        Delta {
            tool_calls: Some(vec![ToolCall {
                index: Some(0),
                function: Some(ToolCallFunction {
                    arguments: Some(":1}".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        },
        None,
        None,
    );

    let (events, rest) = run(&[
        chunk(role, None, None),
        chunk(content("Hel"), None, None),
        chunk(content("lo"), None, None),
        head_tool,
        tail_tool,
        chunk(Delta::default(), Some("tool_calls"), None),
        chunk(Delta::default(), None, Some(usage(5, 4, 2))),
    ]);

    assert_eq!(
        describe(&events),
        vec![
            "message_start",
            "start:0:text",
            "delta:0:text_delta",
            "delta:0:text_delta",
            "stop:0",
            "start:1:tool_use",
            "delta:1:input_json_delta",
            "delta:1:input_json_delta",
            "stop:1",
            "message_delta:tool_use",
            "message_stop",
        ]
    );
    assert!(rest.is_empty());
}

#[test]
fn message_start_carries_the_upstream_identity() {
    let (events, _) = run(&[chunk(content("hi"), None, None)]);

    match &events[0] {
        StreamEvent::MessageStart { message } => {
            assert_eq!(message.id, "chatcmpl-1".to_string());
            assert_eq!(message.role, "assistant".to_string());
            assert_eq!(message.model, "gpt-4o".to_string());
            assert!(message.content.is_empty());
        }
        other => panic!("unexpected first event {other:?}"),
    }
}

#[test]
fn the_finishing_delta_waits_for_the_usage_only_chunk() {
    let (events, tail) = run(&[
        chunk(content("x"), None, None),
        chunk(Delta::default(), Some("stop"), None),
    ]);

    assert_eq!(
        describe(&events),
        vec!["message_start", "start:0:text", "delta:0:text_delta"]
    );
    assert_eq!(
        describe(&tail),
        vec!["stop:0", "message_delta:end_turn", "message_stop"]
    );
}

#[test]
fn usage_on_the_finishing_chunk_closes_in_the_same_push() {
    let (events, tail) = run(&[
        chunk(content("x"), None, None),
        chunk(Delta::default(), Some("length"), Some(usage(7, 3, 0))),
    ]);

    assert_eq!(
        describe(&events)[events.len() - 2..],
        [
            "message_delta:max_tokens".to_string(),
            "message_stop".to_string()
        ]
    );
    assert!(tail.is_empty());
}

#[test]
fn message_delta_reports_unfolded_cache_tokens() {
    let (events, _) = run(&[
        chunk(content("x"), None, None),
        chunk(Delta::default(), Some("stop"), Some(usage(5, 4, 2))),
    ]);

    match &events[events.len() - 2] {
        StreamEvent::MessageDelta { usage, delta } => {
            assert_eq!(usage.input_tokens, 3);
            assert_eq!(usage.output_tokens, 4);
            assert_eq!(usage.cache_read_input_tokens, Some(2));
            assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
        }
        other => panic!("expected message_delta, got {other:?}"),
    }
}

#[test]
fn reasoning_opens_a_thinking_block_before_the_text_block() {
    let (events, _) = run(&[
        chunk(
            Delta {
                reasoning_content: Some("think".into()),
                ..Default::default()
            },
            None,
            None,
        ),
        chunk(
            Delta {
                thinking_blocks: Some(vec![ThinkingBlock {
                    block_type: Some("thinking".into()),
                    thinking: None,
                    signature: Some("sig".into()),
                    data: None,
                }]),
                ..Default::default()
            },
            None,
            None,
        ),
        chunk(content("done"), None, None),
    ]);

    assert_eq!(
        describe(&events),
        vec![
            "message_start",
            "start:0:thinking",
            "delta:0:thinking_delta",
            "delta:0:signature_delta",
            "stop:0",
            "start:1:text",
            "delta:1:text_delta",
        ]
    );
}

#[test]
fn interleaved_tools_get_their_own_blocks_and_client_names() {
    let mut tools = ToolNameMap::new();
    tools.register("get.time", "get_time");
    let mut stream = ChatToAnthropicStream::new("gpt-4o", tools);
    let mut events = Vec::new();
    for slot in 0..2 {
        let call = ToolCall {
            index: Some(slot),
            id: Some(format!("call_{slot}")),
            function: Some(ToolCallFunction {
                name: Some(if slot == 0 { "get_time" } else { "other" }.into()),
                arguments: Some("{}".into()),
            }),
            ..Default::default()
        };
        events.extend(stream.push(&chunk(
            Delta {
                tool_calls: Some(vec![call]),
                ..Default::default()
            },
            None,
            None,
        )));
    }

    assert_eq!(
        describe(&events)[1..],
        [
            "start:0:tool_use".to_string(),
            "delta:0:input_json_delta".to_string(),
            "stop:0".to_string(),
            "start:1:tool_use".to_string(),
            "delta:1:input_json_delta".to_string(),
        ]
    );
    match &events[1] {
        StreamEvent::ContentBlockStart { content_block, .. } => {
            assert_eq!(content_block.name.as_deref(), Some("get.time"));
            assert_eq!(content_block.id.as_deref(), Some("call_0"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn an_empty_upstream_stream_still_emits_a_valid_anthropic_message() {
    let (events, tail) = run(&[]);

    assert!(events.is_empty());
    assert_eq!(
        describe(&tail),
        vec![
            "message_start",
            "start:0:text",
            "stop:0",
            "message_delta:end_turn",
            "message_stop"
        ]
    );
}

#[test]
fn nothing_is_emitted_after_the_message_is_closed() {
    let mut stream = ChatToAnthropicStream::new("gpt-4o", ToolNameMap::new());
    let first = stream.push(&chunk(content("x"), Some("stop"), Some(usage(1, 1, 0))));

    assert_eq!(
        describe(&first).last().map(String::as_str),
        Some("message_stop")
    );
    assert!(stream.push(&chunk(content("late"), None, None)).is_empty());
    assert!(stream.finish().is_empty());
}
