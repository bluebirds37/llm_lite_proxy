use crate::anthropic::{
    Block, BlockDelta, MessageDeltaBody, MessagesResponse, MessagesUsage, StreamEvent,
};
use crate::common::Extras;
use crate::ids::{message_id, token};
use crate::openai::{ChatChunk, ChatUsage, Delta, ToolCall};
use crate::reasons::stop_reason_from_finish;
use crate::tools::ToolNameMap;
use crate::usage::messages_usage_from_chat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Thinking,
    Text,
    Tool,
}

/// Turns OpenAI `chat.completion.chunk` objects into Anthropic Messages SSE
/// events. Anthropic requires an explicit block lifecycle, so every change of
/// delta kind closes the open block and opens the next one.
pub struct ChatToAnthropicStream {
    message_id: String,
    model: String,
    tools: ToolNameMap,
    started: bool,
    block_index: u32,
    open_block: Option<BlockKind>,
    tool_blocks: Vec<(usize, u32)>,
    json_seen: Vec<u32>,
    input_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
    stop_reason: Option<String>,
    usage: Option<ChatUsage>,
    closed: bool,
}

impl ChatToAnthropicStream {
    pub fn new(model: impl Into<String>, tools: ToolNameMap) -> Self {
        Self {
            message_id: message_id(),
            model: model.into(),
            tools,
            started: false,
            block_index: 0,
            open_block: None,
            tool_blocks: Vec::new(),
            json_seen: Vec::new(),
            input_tokens: 0,
            cache_read: 0,
            cache_creation: 0,
            stop_reason: None,
            usage: None,
            closed: false,
        }
    }

    pub fn push(&mut self, chunk: &ChatChunk) -> Vec<StreamEvent> {
        if self.closed {
            return Vec::new();
        }

        let mut events = Vec::new();
        if !self.started {
            if !chunk.model.is_empty() {
                self.model = chunk.model.clone();
            }
            if !chunk.id.is_empty() {
                self.message_id = chunk.id.clone();
            }
            self.capture_usage(chunk.usage.as_ref());
            events.push(self.message_start());
            self.started = true;
        }

        if let Some(usage) = chunk.usage.as_ref() {
            self.capture_usage(Some(usage));
            self.usage = Some(usage.clone());
        }

        for choice in &chunk.choices {
            events.extend(self.delta_events(&choice.delta));
            if let Some(finish) = choice.finish_reason.as_deref() {
                self.stop_reason = Some(stop_reason_from_finish(Some(finish)));
            }
        }

        // A trailing usage-only chunk is what tells us the real output token
        // count, so `message_delta` waits for it instead of guessing.
        if self.stop_reason.is_some() && self.usage.is_some() {
            events.extend(self.message_stop());
        }
        events
    }

    pub fn token_counts(&self) -> (u64, u64) {
        match &self.usage {
            Some(usage) => (usage.prompt_tokens, usage.completion_tokens),
            None => (self.input_tokens, 0),
        }
    }

    pub fn finish(&mut self) -> Vec<StreamEvent> {
        if self.closed {
            return Vec::new();
        }
        if !self.started {
            let mut events = vec![self.message_start()];
            events.push(StreamEvent::ContentBlockStart {
                index: 0,
                content_block: Block::text(String::new()),
            });
            events.push(StreamEvent::ContentBlockStop { index: 0 });
            self.started = true;
            events.extend(self.message_stop());
            return events;
        }
        self.message_stop()
    }

    fn delta_events(&mut self, delta: &Delta) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        for block in delta.thinking_blocks.clone().unwrap_or_default() {
            if let Some(signature) = block.signature.filter(|value| !value.is_empty()) {
                let reuse = self.open_block == Some(BlockKind::Thinking);
                let index = self.next_block_index(reuse, &mut events);
                if !reuse {
                    events.push(StreamEvent::ContentBlockStart {
                        index,
                        content_block: thinking_block(String::new()),
                    });
                }
                self.open_block = Some(BlockKind::Thinking);
                events.push(content_delta("signature_delta", index, signature));
            }
        }

        if let Some(thinking) = delta
            .reasoning_content
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            let reuse = self.open_block == Some(BlockKind::Thinking);
            let index = self.next_block_index(reuse, &mut events);
            if !reuse {
                events.push(StreamEvent::ContentBlockStart {
                    index,
                    content_block: thinking_block(String::new()),
                });
            }
            self.open_block = Some(BlockKind::Thinking);
            events.push(content_delta("thinking_delta", index, thinking.clone()));
        }

        let text = delta
            .content
            .as_ref()
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| delta.refusal.clone().filter(|value| !value.is_empty()));
        if let Some(text) = text {
            let reuse = self.open_block == Some(BlockKind::Text);
            let index = self.next_block_index(reuse, &mut events);
            if !reuse {
                events.push(StreamEvent::ContentBlockStart {
                    index,
                    content_block: Block::text(String::new()),
                });
            }
            self.open_block = Some(BlockKind::Text);
            events.push(content_delta("text_delta", index, text));
        }

        for (position, call) in delta
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            events.extend(self.tool_events(call, position));
        }
        events
    }

    fn tool_events(&mut self, call: ToolCall, position: usize) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let slot = call.index.unwrap_or(position);
        let recorded = self
            .tool_blocks
            .iter()
            .find(|(key, _)| *key == slot)
            .map(|(_, index)| *index);
        let named = call
            .function
            .as_ref()
            .and_then(|f| f.name.clone())
            .unwrap_or_default();

        let reuse = recorded.is_some_and(|index| index == self.block_index)
            && self.open_block == Some(BlockKind::Tool);
        let index = if reuse {
            self.block_index
        } else {
            let index = self.next_block_index(false, &mut events);
            let mut block = Block::typed("tool_use".into());
            if !named.is_empty() {
                block.name = Some(self.tools.client_name(named.as_str()).to_string());
            }
            block.id = Some(
                call.id
                    .clone()
                    .unwrap_or_else(|| format!("toolu_{}", token(20))),
            );
            block.input = Some(serde_json::json!({}));
            events.push(StreamEvent::ContentBlockStart {
                index,
                content_block: block,
            });
            self.open_block = Some(BlockKind::Tool);
            match self.tool_blocks.iter_mut().find(|(key, _)| *key == slot) {
                Some(entry) => entry.1 = index,
                None => self.tool_blocks.push((slot, index)),
            }
            index
        };

        let arguments = call
            .function
            .as_ref()
            .and_then(|function| function.arguments.clone())
            .filter(|value| !value.is_empty());
        if let Some(partial) = arguments {
            if !self.json_seen.contains(&index) {
                self.json_seen.push(index);
            }
            events.push(content_delta("input_json_delta", index, partial));
        }
        events
    }

    /// Closes the block that is open, if any, and hands back the index the next
    /// block should use.
    fn next_block_index(&mut self, reuse: bool, events: &mut Vec<StreamEvent>) -> u32 {
        if reuse {
            return self.block_index;
        }
        if self.open_block.take().is_some() {
            events.push(StreamEvent::ContentBlockStop {
                index: self.block_index,
            });
            self.block_index += 1;
        }
        self.block_index
    }

    fn capture_usage(&mut self, usage: Option<&ChatUsage>) {
        let Some(usage) = usage else {
            return;
        };
        let details = usage.prompt_tokens_details.as_ref();
        self.cache_read = details
            .and_then(|details| details.cached_tokens)
            .unwrap_or(self.cache_read);
        self.cache_creation = details
            .and_then(|details| details.cache_creation_input_tokens)
            .unwrap_or(self.cache_creation);
        self.input_tokens = usage
            .prompt_tokens
            .saturating_sub(self.cache_read)
            .saturating_sub(self.cache_creation)
            .max(self.input_tokens);
    }

    fn message_start(&self) -> StreamEvent {
        StreamEvent::MessageStart {
            message: MessagesResponse {
                id: self.message_id.clone(),
                object: "message".into(),
                role: "assistant".into(),
                model: self.model.clone(),
                content: Vec::new(),
                stop_reason: None,
                stop_sequence: None,
                usage: MessagesUsage {
                    input_tokens: self.input_tokens,
                    output_tokens: 0,
                    cache_creation_input_tokens: (self.cache_creation > 0)
                        .then_some(self.cache_creation),
                    cache_read_input_tokens: (self.cache_read > 0).then_some(self.cache_read),
                    server_tool_use: None,
                    extras: Extras::new(),
                },
                extras: Extras::new(),
            },
        }
    }

    fn message_stop(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if self.open_block.take().is_some() {
            events.push(StreamEvent::ContentBlockStop {
                index: self.block_index,
            });
        }
        if self.stop_reason.is_none() {
            self.stop_reason = Some("end_turn".into());
        }
        let usage = self
            .usage
            .as_ref()
            .map(messages_usage_from_chat)
            .unwrap_or(MessagesUsage {
                input_tokens: self.input_tokens,
                output_tokens: 0,
                cache_read_input_tokens: (self.cache_read > 0).then_some(self.cache_read),
                cache_creation_input_tokens: (self.cache_creation > 0)
                    .then_some(self.cache_creation),
                server_tool_use: None,
                extras: Extras::new(),
            });
        events.push(StreamEvent::MessageDelta {
            delta: MessageDeltaBody {
                stop_reason: self.stop_reason.clone(),
                stop_sequence: None,
                extras: Extras::new(),
            },
            usage,
        });
        events.push(StreamEvent::MessageStop);
        self.closed = true;
        events
    }
}

fn thinking_block(thinking: String) -> Block {
    let mut block = Block::typed("thinking".into());
    block.thinking = Some(thinking);
    block
}

fn content_delta(kind: &str, index: u32, value: String) -> StreamEvent {
    let mut delta = BlockDelta {
        delta_type: kind.to_string(),
        text: None,
        partial_json: None,
        thinking: None,
        signature: None,
        extras: Extras::new(),
    };
    match kind {
        "text_delta" => delta.text = Some(value),
        "thinking_delta" => delta.thinking = Some(value),
        "signature_delta" => delta.signature = Some(value),
        _ => delta.partial_json = Some(value),
    }
    StreamEvent::ContentBlockDelta { index, delta }
}
