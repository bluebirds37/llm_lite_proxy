/// Server-sent framing shared by both directions. Frames are split on a blank
/// line, so a chunk that arrives mid-frame stays in the buffer until it closes.
#[derive(Debug, Clone, PartialEq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

pub const DONE_FRAME: &str = "data: [DONE]\n\n";

pub fn take_frames(buffer: &mut Vec<u8>) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    while let Some((offset, width)) = find_frame_end(buffer) {
        let frame = buffer[..offset].to_vec();
        buffer.drain(..offset + width);
        if let Ok(text) = std::str::from_utf8(&frame) {
            if let Some(parsed) = parse_frame(text) {
                frames.push(parsed);
            }
        }
    }
    frames
}

fn find_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index + 1 < buffer.len() {
        if buffer[index] == b'\r' && buffer[index + 1] == b'\n' {
            if buffer[index + 2..].starts_with(b"\r\n") {
                return Some((index, 4));
            }
            index += 2;
            continue;
        }
        if buffer[index] == b'\n' && buffer[index + 1] == b'\n' {
            return Some((index, 2));
        }
        index += 1;
    }
    None
}

pub fn parse_frame(text: &str) -> Option<SseFrame> {
    let mut event = None;
    let mut data: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data.is_empty() && event.is_none() {
        return None;
    }
    Some(SseFrame {
        event,
        data: data.join("\n"),
    })
}

pub fn encode_openai(json: &str) -> String {
    format!("data: {json}\n\n")
}

pub fn encode_frame(event: Option<&str>, json: &str) -> String {
    match event {
        Some(event) => format!("event: {event}\ndata: {json}\n\n"),
        None => encode_openai(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_on_the_blank_line_and_survive_a_partial_tail() {
        let mut buffer = b"data: {\"a\":1}\n\ndata: {\"b\":2}".to_vec();
        let frames = take_frames(&mut buffer);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "{\"a\":1}".to_string());
        assert_eq!(buffer, b"data: {\"b\":2}".to_vec());

        buffer.extend_from_slice(b"\n\n");
        assert_eq!(take_frames(&mut buffer).len(), 1);
        assert!(buffer.is_empty());
    }

    #[test]
    fn event_and_multiline_data_are_kept_and_comments_dropped() {
        let frame =
            parse_frame("event: content_block_delta\ndata: {\"x\":\ndata: 1}\n: ping comment")
                .unwrap();
        assert_eq!(frame.event.as_deref(), Some("content_block_delta"));
        assert_eq!(frame.data, "{\"x\":\n1}".to_string());
        assert!(parse_frame(": only a comment").is_none());
    }
}
