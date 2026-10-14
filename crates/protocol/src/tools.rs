use std::collections::HashMap;

pub const ANTHROPIC_NAME_MAX: usize = 128;
pub const ANTHROPIC_ID_MAX: usize = 64;
pub const OPENAI_NAME_MAX: usize = 64;

fn allowed(name: char) -> bool {
    name.is_ascii_alphanumeric() || name == '_' || name == '-'
}

fn fnv1a(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |acc, byte| {
        (acc ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub fn sanitize_name(name: &str, max_len: usize) -> String {
    let filtered: String = name
        .chars()
        .map(|c| if allowed(c) { c } else { '_' })
        .collect();
    let filtered = filtered.trim_matches(|c| c == '_' || c == '-');
    if filtered.is_empty() {
        return "tool".into();
    }
    if filtered.len() <= max_len {
        return filtered.to_string();
    }
    let digest = format!("{:08x}", fnv1a(name) & 0xffff_ffff);
    let head = &filtered[..max_len - digest.len() - 1];
    format!("{head}_{digest}")
}

pub fn sanitize_id(id: &str, max_len: usize) -> String {
    let filtered: String = id.chars().filter(|c| allowed(*c)).take(max_len).collect();
    if filtered.is_empty() {
        return format!("id_{}", crate::ids::token(12));
    }
    filtered
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolNameMap {
    to_upstream: HashMap<String, String>,
    to_client: HashMap<String, String>,
}

impl ToolNameMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.to_upstream.is_empty()
    }

    pub fn register(&mut self, client_name: &str, upstream_name: &str) {
        if client_name == upstream_name {
            return;
        }
        self.to_upstream
            .insert(client_name.to_string(), upstream_name.to_string());
        self.to_client
            .insert(upstream_name.to_string(), client_name.to_string());
    }

    pub fn upstream_name<'a>(&'a self, client_name: &'a str) -> &'a str {
        self.to_upstream
            .get(client_name)
            .map(String::as_str)
            .unwrap_or(client_name)
    }

    pub fn client_name<'a>(&'a self, upstream_name: &'a str) -> &'a str {
        self.to_client
            .get(upstream_name)
            .map(String::as_str)
            .unwrap_or(upstream_name)
    }
}
