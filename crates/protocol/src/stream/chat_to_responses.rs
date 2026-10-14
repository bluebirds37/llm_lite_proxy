use std::collections::HashMap;

use serde_json::Value;

use crate::common::Extras;
use crate::ids::{call_id, created, token};
use crate::openai::{ChatChunk, ChatUsage, Delta, ToolCall};
use crate::openai_responses::{
    events, ContentPart as ResponsesPart, OutputItem, ResponsesEvent, ResponsesResponse,
    ResponsesUsage, UsageDetails,
};
use crate::tools::ToolNameMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    Reasoning,
    Message,
    Refusal,
    Tool,
}

#[derive(Debug, Clone)]
struct OpenItem {
    kind: OpenKind,
    item_id: String,
    output_index: u32,
    call_id: Option<String>,
    name: String,
    text: String,
}

/// Turns Chat Completions chunks into OpenAI Responses SSE events. Responses
/// requires an explicit item and part lifecycle, so a change of delta kind
/// closes the open item before the next one is added.
pub struct ChatToResponsesStream {
    response_id: String,
    model: String,
    created: u64,
    sequence: u64,
    tools: ToolNameMap,
    started: bool,
    closed: bool,
    next_index: u32,
    open: Option<OpenItem>,
    tool_slots: HashMap<usize, u32>,
    tool_arguments: HashMap<u32, String>,
    output: Vec<OutputItem>,
    usage: Option<ChatUsage>,
    finish_reason: Option<String>,
}

impl ChatToResponsesStream {
    pub fn new(model: impl Into<String>, tools: ToolNameMap) -> Self {
        Self {
            response_id: format!("resp_{}", token(24)),
            model: model.into(),
            created: created(),
            sequence: 0,
            tools,
            started: false,
            closed: false,
            next_index: 0,
            open: None,
            tool_slots: HashMap::new(),
            tool_arguments: HashMap::new(),
            output: Vec::new(),
            usage: None,
            finish_reason: None,
        }
    }

    pub fn push(&mut self, chunk: &ChatChunk) -> Vec<ResponsesEvent> {
        if self.closed {
            return Vec::new();
        }
        let mut out = Vec::new();

        if !self.started {
            if !chunk.model.is_empty() {
                self.model = chunk.model.clone();
            }
            if !chunk.id.is_empty() {
                self.response_id = chunk.id.clone();
            }
            if chunk.created > 0 {
                self.created = chunk.created;
            }
            self.started = true;
            out.push(self.lifecycle(events::CREATED, "in_progress", None));
            out.push(self.lifecycle(events::IN_PROGRESS, "in_progress", None));
        }
        if let Some(usage) = &chunk.usage {
            self.usage = Some(usage.clone());
        }

        for choice in &chunk.choices {
            out.extend(self.delta_events(&choice.delta));
            if let Some(finish) = choice.finish_reason.as_deref() {
                self.finish_reason = Some(finish.to_string());
            }
        }

        // `response.completed` carries the usage, so it waits for the trailing
        // usage-only chunk the same way the Anthropic bridge does.
        if self.finish_reason.is_some() && self.usage.is_some() {
            out.extend(self.completed());
        }
        out
    }

    pub fn finish(&mut self) -> Vec<ResponsesEvent> {
        if self.closed {
            return Vec::new();
        }
        if !self.started {
            self.started = true;
            let mut out = vec![
                self.lifecycle(events::CREATED, "in_progress", None),
                self.lifecycle(events::IN_PROGRESS, "in_progress", None),
            ];
            out.extend(self.completed());
            return out;
        }
        self.completed()
    }

    /// Ends the stream with `response.failed` instead of pretending the partial
    /// output was the whole answer.
    pub fn failure(&mut self, message: &str) -> Vec<ResponsesEvent> {
        if self.closed {
            return Vec::new();
        }
        let mut out = Vec::new();
        if !self.started {
            out.push(self.lifecycle(events::CREATED, "in_progress", None));
            self.started = true;
        }
        self.close_open(&mut out);
        out.push(self.lifecycle(events::FAILED, "failed", Some(message)));
        self.closed = true;
        out
    }

    pub fn token_counts(&self) -> (u64, u64) {
        match &self.usage {
            Some(usage) => (usage.prompt_tokens, usage.completion_tokens),
            None => (0, 0),
        }
    }

    fn delta_events(&mut self, delta: &Delta) -> Vec<ResponsesEvent> {
        let mut out = Vec::new();

        if let Some(reasoning) = delta
            .reasoning_content
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            out.extend(self.append(OpenKind::Reasoning, reasoning));
        }
        if let Some(text) = delta
            .content
            .as_ref()
            .filter(|value| !value.is_empty())
            .cloned()
        {
            out.extend(self.append(OpenKind::Message, &text));
        }
        if let Some(refusal) = delta.refusal.as_ref().filter(|value| !value.is_empty()) {
            out.extend(self.append(OpenKind::Refusal, refusal));
        }

        for (position, call) in delta
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            out.extend(self.tool_events(call, position));
        }
        out
    }

    fn append(&mut self, kind: OpenKind, text: &str) -> Vec<ResponsesEvent> {
        let mut out = Vec::new();
        if self.open.as_ref().is_some_and(|open| open.kind != kind) {
            self.close_open(&mut out);
        }
        if self.open.is_none() {
            self.open_item(kind, &mut out);
        }
        let Some(open) = self.open.as_mut() else {
            return out;
        };
        open.text.push_str(text);
        let (item_id, index) = (open.item_id.clone(), open.output_index);

        let mut event = match kind {
            OpenKind::Reasoning => {
                let mut event = self.event(events::REASONING_SUMMARY_TEXT_DELTA);
                event.summary_index = Some(0);
                event
            }
            OpenKind::Refusal => {
                let mut event = self.event("response.refusal.delta");
                event.content_index = Some(0);
                event
            }
            _ => {
                let mut event = self.event(events::OUTPUT_TEXT_DELTA);
                event.content_index = Some(0);
                event
            }
        };
        event.item_id = Some(item_id);
        event.output_index = Some(index);
        event.delta = Some(text.to_string());
        out.push(event);
        out
    }

    fn open_item(&mut self, kind: OpenKind, out: &mut Vec<ResponsesEvent>) {
        let index = self.next_index;
        self.next_index += 1;
        let item_id = match kind {
            OpenKind::Reasoning => format!("rs_{}", token(20)),
            _ => format!("msg_{}", token(20)),
        };

        let item = match kind {
            OpenKind::Reasoning => OutputItem {
                item_type: "reasoning".into(),
                id: Some(item_id.clone()),
                summary: Some(Vec::new()),
                ..Default::default()
            },
            _ => OutputItem {
                item_type: "message".into(),
                id: Some(item_id.clone()),
                role: Some("assistant".into()),
                status: Some("in_progress".into()),
                content: Some(Vec::new()),
                ..Default::default()
            },
        };
        out.push({
            let mut event = self.event(events::OUTPUT_ITEM_ADDED);
            event.output_index = Some(index);
            event.item_id = Some(item_id.clone());
            event.item = Some(item);
            event
        });

        match kind {
            OpenKind::Reasoning => {
                out.push({
                    let mut event = self.event(events::REASONING_SUMMARY_PART_ADDED);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(index);
                    event.summary_index = Some(0);
                    event.part = Some(summary_part(""));
                    event
                });
            }
            OpenKind::Refusal => {
                out.push({
                    let mut event = self.event(events::CONTENT_PART_ADDED);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(index);
                    event.content_index = Some(0);
                    event.part = Some(ResponsesPart {
                        part_type: "refusal".into(),
                        ..Default::default()
                    });
                    event
                });
            }
            _ => {
                out.push({
                    let mut event = self.event(events::CONTENT_PART_ADDED);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(index);
                    event.content_index = Some(0);
                    event.part = Some(ResponsesPart::output_text(String::new()));
                    event
                });
            }
        };

        self.open = Some(OpenItem {
            kind,
            item_id,
            output_index: index,
            call_id: None,
            name: String::new(),
            text: String::new(),
        });
    }

    fn tool_events(&mut self, call: ToolCall, position: usize) -> Vec<ResponsesEvent> {
        let mut out = Vec::new();
        let slot = call.index.unwrap_or(position);
        let function = call.function.unwrap_or_default();
        let declared = function.name.unwrap_or_default();
        let arguments = function.arguments.unwrap_or_default();

        let index = match self.tool_slots.get(&slot).copied() {
            Some(index) => index,
            None => {
                if self.open.is_some() {
                    self.close_open(&mut out);
                }
                let index = self.next_index;
                self.next_index += 1;
                let call_id = call.id.clone().unwrap_or_else(call_id);
                let item_id = format!("fc_{call_id}");
                let name = self.tools.client_name(&declared).to_string();
                out.push({
                    let mut event = self.event(events::OUTPUT_ITEM_ADDED);
                    event.output_index = Some(index);
                    event.item_id = Some(item_id.clone());
                    event.item = Some(OutputItem::function_call(&item_id, &call_id, &name));
                    event
                });
                self.open = Some(OpenItem {
                    kind: OpenKind::Tool,
                    item_id,
                    output_index: index,
                    call_id: Some(call_id),
                    name,
                    text: String::new(),
                });
                self.tool_slots.insert(slot, index);
                index
            }
        };

        if arguments.is_empty() {
            return out;
        }
        *self.tool_arguments.entry(index).or_default() += &arguments;
        let item_id = self
            .open
            .as_ref()
            .filter(|open| open.output_index == index)
            .map(|open| open.item_id.clone())
            .unwrap_or_else(|| format!("fc_{}", self.tool_slots.get(&slot).cloned().unwrap_or(0)));
        out.push({
            let mut event = self.event(events::FUNCTION_CALL_ARGS_DELTA);
            event.item_id = Some(item_id);
            event.output_index = Some(index);
            event.delta = Some(arguments);
            event
        });
        out
    }

    fn close_open(&mut self, out: &mut Vec<ResponsesEvent>) {
        let Some(open) = self.open.take() else {
            return;
        };
        let OpenItem {
            kind,
            item_id,
            output_index,
            call_id,
            name,
            text,
        } = open;

        let finished = match kind {
            OpenKind::Reasoning => {
                out.push({
                    let mut event = self.event(events::REASONING_SUMMARY_TEXT_DONE);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(output_index);
                    event.summary_index = Some(0);
                    event.text = Some(text.clone());
                    event
                });
                out.push({
                    let mut event = self.event(events::REASONING_SUMMARY_PART_DONE);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(output_index);
                    event.summary_index = Some(0);
                    event.part = Some(summary_part(&text));
                    event
                });
                OutputItem {
                    item_type: "reasoning".into(),
                    id: Some(item_id.clone()),
                    summary: Some(vec![summary_value(&text)]),
                    ..Default::default()
                }
            }
            OpenKind::Tool => {
                let arguments = self
                    .tool_arguments
                    .remove(&output_index)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "{}".to_string());
                out.push({
                    let mut event = self.event(events::FUNCTION_CALL_ARGS_DONE);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(output_index);
                    event.arguments = Some(arguments.clone());
                    event
                });
                let mut item =
                    OutputItem::function_call(&item_id, call_id.unwrap_or(item_id.clone()), &name);
                item.arguments = Some(arguments);
                item
            }
            OpenKind::Message | OpenKind::Refusal => {
                let refusal = kind == OpenKind::Refusal;
                out.push({
                    let mut event = if refusal {
                        let mut event = self.event("response.refusal.done");
                        event.refusal = Some(text.clone());
                        event
                    } else {
                        let mut event = self.event(events::OUTPUT_TEXT_DONE);
                        event.text = Some(text.clone());
                        event
                    };
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(output_index);
                    event.content_index = Some(0);
                    event
                });
                out.push({
                    let mut event = self.event(events::CONTENT_PART_DONE);
                    event.item_id = Some(item_id.clone());
                    event.output_index = Some(output_index);
                    event.content_index = Some(0);
                    event.part = Some(part_of(refusal, &text));
                    event
                });
                OutputItem {
                    item_type: "message".into(),
                    id: Some(item_id.clone()),
                    role: Some("assistant".into()),
                    status: Some("completed".into()),
                    content: Some(vec![part_of(refusal, &text)]),
                    ..Default::default()
                }
            }
        };

        out.push({
            let mut event = self.event(events::OUTPUT_ITEM_DONE);
            event.output_index = Some(output_index);
            event.item_id = Some(item_id);
            event.item = Some(finished.clone());
            event
        });
        self.output.push(finished);
    }

    fn completed(&mut self) -> Vec<ResponsesEvent> {
        let mut out = Vec::new();
        self.close_open(&mut out);
        let truncated = self.finish_reason.as_deref() == Some("length");
        let content_filter = self.finish_reason.as_deref() == Some("content_filter");
        let kind = if truncated || content_filter {
            events::INCOMPLETE
        } else {
            events::COMPLETED
        };
        let status = if truncated || content_filter {
            "incomplete"
        } else {
            "completed"
        };
        out.push(self.lifecycle(kind, status, None));
        self.closed = true;
        out
    }

    fn lifecycle(&mut self, kind: &str, status: &str, error: Option<&str>) -> ResponsesEvent {
        let mut event = self.event(kind);
        let response = ResponsesResponse {
            id: self.response_id.clone(),
            object: "response".into(),
            created_at: self.created,
            status: status.into(),
            model: self.model.clone(),
            output: self.output.clone(),
            usage: self.usage.clone().map(responses_usage_of),
            error: error.map(|message| {
                let mut body = Extras::new();
                body.insert("message".into(), Value::String(message.to_string()));
                Value::Object(body)
            }),
            incomplete_details: (status == "incomplete").then(|| {
                let mut reason = Extras::new();
                reason.insert(
                    "reason".into(),
                    Value::String(if self.finish_reason.as_deref() == Some("content_filter") {
                        "content_filter".to_string()
                    } else {
                        "max_output_tokens".to_string()
                    }),
                );
                Value::Object(reason)
            }),
            instructions: None,
            completed_at: (status == "completed").then_some(self.created),
            sequence_number: event.sequence_number,
            extras: Extras::new(),
        };
        event.response = Some(response);
        event
    }

    fn event(&mut self, kind: &str) -> ResponsesEvent {
        self.sequence += 1;
        ResponsesEvent::new(kind, self.sequence)
    }
}

fn part_of(refusal: bool, text: &str) -> ResponsesPart {
    if refusal {
        ResponsesPart {
            part_type: "refusal".into(),
            refusal: Some(text.to_string()),
            ..Default::default()
        }
    } else {
        ResponsesPart {
            part_type: "output_text".into(),
            text: Some(text.to_string()),
            annotations: Some(Vec::new()),
            ..Default::default()
        }
    }
}

fn summary_part(text: &str) -> ResponsesPart {
    ResponsesPart {
        part_type: "summary_text".into(),
        text: Some(text.to_string()),
        ..Default::default()
    }
}

fn summary_value(text: &str) -> Value {
    let mut part = Extras::new();
    part.insert("type".into(), Value::String("summary_text".into()));
    part.insert("text".into(), Value::String(text.to_string()));
    Value::Object(part)
}

fn responses_usage_of(usage: ChatUsage) -> ResponsesUsage {
    ResponsesUsage {
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
        extras: usage.extras.clone(),
    }
}
