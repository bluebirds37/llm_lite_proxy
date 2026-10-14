use crate::anthropic::MessagesUsage;
use crate::openai::{ChatUsage, UsageDetails};
use serde_json::Value;

fn nested_total(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_object)
        .map(|object| object.values().filter_map(Value::as_u64).sum::<u64>())
        .unwrap_or_default()
}

pub fn cache_creation_total(usage: &MessagesUsage) -> u64 {
    usage
        .cache_creation_input_tokens
        .or_else(|| {
            usage
                .extras
                .get("cache_creation")
                .map(|nested| nested_total(Some(nested)))
        })
        .unwrap_or_default()
}

pub fn cache_read_total(usage: &MessagesUsage) -> u64 {
    usage.cache_read_input_tokens.unwrap_or_default()
}

/// Anthropic reports the cache buckets as disjoint from `input_tokens`, while
/// OpenAI's `prompt_tokens` is the whole prompt, so both buckets fold into it.
pub fn chat_usage_from_messages(usage: &MessagesUsage, reasoning_tokens: u64) -> ChatUsage {
    let cache_read = cache_read_total(usage);
    let cache_creation = cache_creation_total(usage);
    let mut prompt_tokens_details = UsageDetails::default();
    if cache_read > 0 {
        prompt_tokens_details.cached_tokens = Some(cache_read);
    }
    if cache_creation > 0 {
        prompt_tokens_details.cache_creation_input_tokens = Some(cache_creation);
        if let Some(nested) = usage.extras.get("cache_creation") {
            prompt_tokens_details.cache_creation = Some(nested.clone());
        }
    }

    let completion_tokens_details = (reasoning_tokens > 0).then(|| UsageDetails {
        reasoning_tokens: Some(reasoning_tokens),
        ..Default::default()
    });

    ChatUsage {
        prompt_tokens: usage.input_tokens + cache_read + cache_creation,
        completion_tokens: usage.output_tokens,
        total_tokens: usage.input_tokens + cache_read + cache_creation + usage.output_tokens,
        prompt_tokens_details: (cache_read > 0 || cache_creation > 0)
            .then_some(prompt_tokens_details),
        completion_tokens_details,
        extras: usage.extras.clone(),
    }
}

pub fn messages_usage_from_chat(usage: &ChatUsage) -> MessagesUsage {
    let (cache_read, cache_creation) = match usage.prompt_tokens_details.as_ref() {
        Some(details) => (
            details.cached_tokens.unwrap_or_default(),
            details
                .cache_creation_input_tokens
                .unwrap_or_else(|| nested_total(details.cache_creation.as_ref())),
        ),
        None => (0, 0),
    };
    let input_tokens = usage
        .prompt_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_creation);

    MessagesUsage {
        input_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_input_tokens: (cache_read > 0).then_some(cache_read),
        cache_creation_input_tokens: (cache_creation > 0).then_some(cache_creation),
        server_tool_use: usage.extras.get("server_tool_use").cloned(),
        extras: usage.extras.clone(),
    }
}
