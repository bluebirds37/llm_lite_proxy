use crate::anthropic::MessagesUsage;
use crate::common::Extras;
use crate::ids::{call_id, chat_id, created};
use crate::openai::{ChatChunk, ChunkChoice, Delta, ToolCall, ToolCallFunction};
use crate::openai_responses::{ResponsesEvent, ResponsesUsage};
use crate::tools::ToolNameMap;
use crate::usage::chat_usage_from_messages;

/// Turns OpenAI Responses SSE events into Chat Completions chunks.
pub struct ResponsesToChatStream {
    id: String,
    model: String,
    created: u64,
    tools: ToolNameMap,
    usage: ResponsesUsage,
    calls: Vec<(String, usize, bool)>,
    call_counter: usize,
    has_calls: bool,
    finish_reason: Option<String>,
    started: bool,
    finished: bool,
}

impl ResponsesToChatStream {
    pub fn new(model: impl Into<String>, tools: ToolNameMap) -> Self {
        Self {
            id: chat_id(),
            model: model.into(),
            created: created(),
            tools,
            usage: ResponsesUsage::default(),
            calls: Vec::new(),
            call_counter: 0,
            has_calls: false,
            finish_reason: None,
            started: false,
            finished: false,
        }
    }

    pub fn push(&mut self, event: &ResponsesEvent) -> Vec<ChatChunk> {
        if self.finished {
            return Vec::new();
        }
        match event.event_type.as_str() {
            "response.created" | "response.in_progress" => {
                if let Some(response) = &event.response {
                    if !response.model.is_empty() {
                        self.model = response.model.clone();
                    }
                    if !response.id.is_empty() {
                        self.id = response.id.clone();
                    }
                    if response.created_at > 0 {
                        self.created = response.created_at;
                    }
                    if let Some(usage) = response.usage.as_ref() {
                        self.usage = usage.clone();
                    }
                }
                if self.started {
                    Vec::new()
                } else {
                    self.started = true;
                    self.chunk(Delta {
                        role: Some("assistant".into()),
                        content: Some(String::new()),
                        ..Default::default()
                    })
                }
            }
            "response.output_item.added" => self.item_added(event),
            "response.output_text.delta" => self.text(&event.delta),
            "response.refusal.delta" => self.chunk(Delta {
                refusal: event.delta.clone(),
                ..Default::default()
            }),
            "response.reasoning_summary_text.delta" => self.chunk(Delta {
                reasoning_content: event.delta.clone(),
                ..Default::default()
            }),
            "response.function_call_arguments.delta" => self.arguments(event),
            "response.output_item.done" => self.item_done(event),
            "response.completed" => {
                self.capture_usage(event);
                self.close(None)
            }
            "response.incomplete" => {
                self.capture_usage(event);
                self.close(incomplete_finish(event))
            }
            "response.failed" | "response.cancelled" => {
                self.finished = true;
                let message = event
                    .response
                    .as_ref()
                    .and_then(|response| response.error.as_ref())
                    .map(crate::common::json_message)
                    .unwrap_or_else(|| "the upstream response failed".to_string());
                vec![self.error_chunk("server_error", &message)]
            }
            "error" => {
                self.finished = true;
                let code = event.code.clone().unwrap_or_else(|| "server_error".into());
                let message = event
                    .message
                    .clone()
                    .or_else(|| event.error.as_ref().map(crate::common::json_message))
                    .unwrap_or_else(|| "the upstream stream failed".to_string());
                vec![self.error_chunk(&code, &message)]
            }
            _ => Vec::new(),
        }
    }

    pub fn finish(&mut self) -> Vec<ChatChunk> {
        self.close(None)
    }

    pub fn token_counts(&self) -> (u64, u64) {
        let usage = self.chat_usage();
        (usage.prompt_tokens, usage.completion_tokens)
    }

    fn item_added(&mut self, event: &ResponsesEvent) -> Vec<ChatChunk> {
        let Some(item) = &event.item else {
            return Vec::new();
        };
        if item.item_type != "function_call" {
            return Vec::new();
        }
        self.has_calls = true;
        let slot = self.call_counter;
        self.call_counter += 1;
        let item_id = item
            .id
            .clone()
            .or_else(|| event.item_id.clone())
            .unwrap_or_else(call_id);
        let call_id = item.call_id.clone().unwrap_or_else(|| item_id.clone());
        self.calls.push((item_id, slot, false));
        let name = self
            .tools
            .client_name(item.name.as_deref().unwrap_or_default())
            .to_string();
        self.chunk(Delta {
            tool_calls: Some(vec![ToolCall {
                index: Some(slot),
                id: Some(call_id),
                call_type: Some("function".into()),
                function: Some(ToolCallFunction {
                    name: Some(name),
                    arguments: Some(String::new()),
                }),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }

    fn arguments(&mut self, event: &ResponsesEvent) -> Vec<ChatChunk> {
        let Some(partial) = event.delta.as_ref().filter(|value| !value.is_empty()) else {
            return Vec::new();
        };
        let item_id = event.item_id.clone().unwrap_or_default();
        let slot = match self
            .calls
            .iter()
            .position(|(known, _, _)| *known == item_id)
        {
            Some(at) => {
                self.calls[at].2 = true;
                self.calls[at].1
            }
            None => {
                let slot = self.call_counter;
                self.call_counter += 1;
                self.calls.push((item_id, slot, true));
                slot
            }
        };
        self.chunk(Delta {
            tool_calls: Some(vec![ToolCall {
                index: Some(slot),
                function: Some(ToolCallFunction {
                    arguments: Some(partial.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }

    /// A tool that received no argument deltas would otherwise leave the client
    /// with an empty argument string instead of a parseable object.
    fn item_done(&mut self, event: &ResponsesEvent) -> Vec<ChatChunk> {
        let Some(item) = &event.item else {
            return Vec::new();
        };
        if item.item_type != "function_call" {
            return Vec::new();
        }
        let item_id = item
            .id
            .clone()
            .or_else(|| event.item_id.clone())
            .unwrap_or_default();
        let Some((_, slot, saw_arguments)) = self
            .calls
            .iter()
            .find(|(known, _, _)| *known == item_id)
            .cloned()
        else {
            return Vec::new();
        };
        if saw_arguments {
            return Vec::new();
        }
        self.chunk(Delta {
            tool_calls: Some(vec![ToolCall {
                index: Some(slot),
                function: Some(ToolCallFunction {
                    arguments: Some("{}".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }

    fn text(&mut self, delta: &Option<String>) -> Vec<ChatChunk> {
        match delta.as_ref().filter(|value| !value.is_empty()) {
            Some(value) => self.chunk(Delta {
                content: Some(value.clone()),
                ..Default::default()
            }),
            None => Vec::new(),
        }
    }

    fn capture_usage(&mut self, event: &ResponsesEvent) {
        let Some(usage) = event
            .response
            .as_ref()
            .and_then(|response| response.usage.as_ref())
        else {
            return;
        };
        self.usage = usage.clone();
    }

    fn close(&mut self, fallback: Option<&str>) -> Vec<ChatChunk> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let finish_reason = fallback
            .map(str::to_string)
            .or_else(|| self.finish_reason.clone())
            .unwrap_or_else(|| {
                if self.has_calls {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                }
            });
        vec![
            ChatChunk {
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: Delta::default(),
                    finish_reason: Some(finish_reason),
                }],
                ..self.envelope()
            },
            ChatChunk {
                usage: Some(self.chat_usage()),
                ..self.envelope()
            },
        ]
    }

    fn chat_usage(&self) -> crate::openai::ChatUsage {
        let reasoning = self
            .usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens)
            .unwrap_or_default();
        let folded = MessagesUsage {
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_input_tokens: self
                .usage
                .input_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
            cache_creation_input_tokens: None,
            server_tool_use: None,
            extras: Extras::new(),
        };
        let mut usage = chat_usage_from_messages(&folded, reasoning);
        // Responses already counts cached tokens inside `input_tokens`, so undo
        // the Anthropic-style folding done by the shared helper.
        if let Some(cached) = folded.cache_read_input_tokens {
            usage.prompt_tokens = usage.prompt_tokens.saturating_sub(cached);
            usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
            usage.prompt_tokens_details = Some(crate::openai::UsageDetails {
                cached_tokens: Some(cached),
                ..Default::default()
            });
        }
        usage.total_tokens = if self.usage.total_tokens > 0 {
            self.usage.total_tokens
        } else {
            usage.total_tokens
        };
        usage
    }

    fn chunk(&mut self, delta: Delta) -> Vec<ChatChunk> {
        self.started = true;
        vec![ChatChunk {
            choices: vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason: None,
            }],
            ..self.envelope()
        }]
    }

    fn envelope(&self) -> ChatChunk {
        ChatChunk {
            id: self.id.clone(),
            object: "chat.completion.chunk".into(),
            created: self.created,
            model: self.model.clone(),
            choices: Vec::new(),
            usage: None,
            extras: Extras::new(),
        }
    }

    fn error_chunk(&self, code: &str, message: &str) -> ChatChunk {
        let mut chunk = self.envelope();
        chunk.extras.insert(
            "error".into(),
            serde_json::json!({"message": message, "type": code}),
        );
        chunk
    }
}

fn incomplete_finish(event: &ResponsesEvent) -> Option<&'static str> {
    let reason = event
        .response
        .as_ref()
        .and_then(|response| response.incomplete_details.as_ref())
        .and_then(|details| details.get("reason"))
        .and_then(|reason| reason.as_str());
    match reason {
        Some("content_filter") => Some("content_filter"),
        _ => Some("length"),
    }
}
