pub fn finish_reason_from_stop_reason(stop_reason: Option<&str>) -> String {
    match stop_reason {
        Some("max_tokens") | Some("compaction") => "length",
        Some("tool_use") | Some("server_tool_use") | Some("function_call") => "tool_calls",
        Some("refusal") | Some("content_filter") => "content_filter",
        _ => "stop",
    }
    .to_string()
}

pub fn stop_reason_from_finish(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("content_filter") | Some("refusal") => "refusal",
        _ => "end_turn",
    }
    .to_string()
}
