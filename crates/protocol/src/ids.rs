use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn created() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

fn mix(value: u64) -> u64 {
    let mut x = value.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^ (x >> 31)
}

fn entropy() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let salt = &COUNTER as *const AtomicU64 as u64;
    mix(since.as_secs()
        ^ (since.subsec_nanos() as u64) << 17
        ^ COUNTER.fetch_add(1, Ordering::Relaxed) << 9
        ^ salt)
}

pub fn token(len: usize) -> String {
    let mut out = String::with_capacity(len);
    loop {
        out.push_str(&format!("{:016x}", entropy()));
        if out.len() >= len {
            out.truncate(len);
            return out;
        }
    }
}

pub fn chat_id() -> String {
    format!("chatcmpl-{}", token(21))
}

pub fn message_id() -> String {
    format!("msg_{}", token(24).to_uppercase())
}

pub fn call_id() -> String {
    format!("call_{}", token(24))
}
