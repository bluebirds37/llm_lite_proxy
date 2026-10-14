use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::config::Api;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: u64,
    pub at_ms: u64,
    pub client: String,
    pub endpoint: String,
    pub provider: String,
    pub upstream_protocol: String,
    pub requested_model: String,
    pub upstream_model: String,
    pub converted: bool,
    pub streaming: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub first_token_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub error: Option<String>,
    /// Raw upstream body, kept for the log panel and never sent to the client.
    pub detail: Option<String>,
    pub dropped: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub active: u64,
    pub uptime_ms: u64,
    pub by_provider: HashMap<String, ProviderStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderStats {
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub avg_latency_ms: u64,
}

#[derive(Debug)]
struct Inner {
    capacity: AtomicUsize,
    started_at: u64,
    next_id: AtomicU64,
    entries: Mutex<VecDeque<LogEntry>>,
    stats: Mutex<Stats>,
    latency_total: Mutex<HashMap<String, u64>>,
    sender: broadcast::Sender<LogEntry>,
}

/// Ring buffer of finished requests plus aggregate counters. Cheap to clone so
/// every handler and stream can hold one.
#[derive(Debug, Clone)]
pub struct LogBus {
    inner: Arc<Inner>,
    active: Arc<AtomicU64>,
}

impl LogBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(512);
        Self {
            inner: Arc::new(Inner {
                capacity: AtomicUsize::new(capacity.clamp(16, 5_000)),
                started_at: epoch_ms(),
                next_id: AtomicU64::new(1),
                entries: Mutex::new(VecDeque::new()),
                stats: Mutex::new(Stats::default()),
                latency_total: Mutex::new(HashMap::new()),
                sender,
            }),
            active: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn record(&self, mut entry: LogEntry) {
        entry.id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        entry.latency_ms = entry.latency_ms.max(1);

        let capacity = self.inner.capacity.load(Ordering::Relaxed);
        let mut entries = self.inner.entries.lock().expect("log entries lock");
        while entries.len() >= capacity {
            entries.pop_front();
        }
        entries.push_back(entry.clone());
        drop(entries);

        let mut stats = self.inner.stats.lock().expect("log stats lock");
        stats.requests += 1;
        stats.input_tokens += entry.input_tokens;
        stats.output_tokens += entry.output_tokens;
        if entry.status >= 400 {
            stats.errors += 1;
        }
        let provider = stats.by_provider.entry(entry.provider.clone()).or_default();
        provider.requests += 1;
        provider.input_tokens += entry.input_tokens;
        provider.output_tokens += entry.output_tokens;
        if entry.status >= 400 {
            provider.errors += 1;
        }
        drop(stats);

        let mut totals = self
            .inner
            .latency_total
            .lock()
            .expect("latency totals lock");
        let total = totals.entry(entry.provider.clone()).or_insert(0);
        *total += entry.latency_ms;
        let average = *total;
        drop(totals);

        let mut stats = self.inner.stats.lock().expect("log stats lock");
        if let Some(provider) = stats.by_provider.get_mut(&entry.provider) {
            provider.avg_latency_ms = average / provider.requests.max(1);
        }
        drop(stats);

        let _ = self.inner.sender.send(entry);
    }

    /// Takes effect immediately so the setting is never a silent no-op, and the
    /// panel keeps its history because the buffer is not recreated.
    pub fn set_capacity(&self, capacity: usize) {
        self.inner
            .capacity
            .store(capacity.clamp(16, 5_000), Ordering::Relaxed);
    }

    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let entries = self.inner.entries.lock().expect("log entries lock");
        let skip = entries.len().saturating_sub(limit);
        entries.iter().skip(skip).rev().cloned().collect()
    }

    pub fn stats(&self) -> Stats {
        let mut stats = self.inner.stats.lock().expect("log stats lock").clone();
        stats.active = self.active.load(Ordering::Relaxed);
        stats.uptime_ms = epoch_ms().saturating_sub(self.inner.started_at);
        stats
    }

    /// Requests rejected before routing (bad key, unparseable body) still need
    /// to be visible in the panel.
    pub fn record_rejected(&self, client: Api, endpoint: &str, status: u16, message: &str) {
        let mut entry = LogEntry {
            id: 0,
            at_ms: epoch_ms(),
            client: client.as_str().to_string(),
            endpoint: endpoint.to_string(),
            provider: String::new(),
            upstream_protocol: String::new(),
            requested_model: String::new(),
            upstream_model: String::new(),
            converted: false,
            streaming: false,
            status,
            latency_ms: 1,
            first_token_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            error: Some(message.to_string()),
            detail: None,
            dropped: Vec::new(),
            notes: Vec::new(),
        };
        entry.notes.push("rejected before routing".to_string());
        self.record(entry);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.inner.sender.subscribe()
    }

    pub fn begin(&self) -> ActiveGuard {
        self.active.fetch_add(1, Ordering::Relaxed);
        ActiveGuard {
            counter: Arc::clone(&self.active),
        }
    }
}

pub struct ActiveGuard {
    counter: Arc<AtomicU64>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Mutable view of one request's log entry, shared between the handler and the
/// body it streams out.
#[derive(Debug, Clone)]
pub struct TraceHandle {
    entry: Arc<Mutex<LogEntry>>,
    started: Instant,
}

impl TraceHandle {
    pub fn route(&self, provider: &str, upstream_protocol: &str, model: &str, converted: bool) {
        let mut entry = self.entry.lock().expect("trace lock");
        entry.provider = provider.to_string();
        entry.upstream_protocol = upstream_protocol.to_string();
        entry.upstream_model = model.to_string();
        entry.converted = converted;
    }

    pub fn dropped(&self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        self.entry.lock().expect("trace lock").dropped = names.to_vec();
    }

    pub fn note(&self, note: &str) {
        let mut entry = self.entry.lock().expect("trace lock");
        if !entry.notes.iter().any(|seen| seen == note) {
            entry.notes.push(note.to_string());
        }
    }

    pub fn streaming(&self) {
        self.entry.lock().expect("trace lock").streaming = true;
    }

    pub fn first_token(&self) {
        let mut entry = self.entry.lock().expect("trace lock");
        entry
            .first_token_ms
            .get_or_insert_with(|| elapsed(self.started));
    }

    pub fn tokens(&self, input: u64, output: u64) {
        let mut entry = self.entry.lock().expect("trace lock");
        entry.input_tokens = entry.input_tokens.max(input);
        entry.output_tokens = entry.output_tokens.max(output);
    }

    pub fn status(&self, status: u16, error: Option<String>, detail: Option<String>) {
        let mut entry = self.entry.lock().expect("trace lock");
        entry.status = status;
        entry.error = error;
        entry.detail = detail;
    }
}

/// Owns one request from admission to the last streamed byte. Dropping it
/// without `complete` means the handler bailed early or the client hung up,
/// which is still recorded so nothing disappears from the panel.
pub struct Trace {
    inner: Option<StreamedTrace>,
}

impl Trace {
    pub fn new(bus: LogBus, client: Api, endpoint: &str, requested_model: &str) -> Self {
        let active = bus.begin();
        Self {
            inner: Some(StreamedTrace {
                bus,
                _active: active,
                started: Instant::now(),
                entry: Arc::new(Mutex::new(LogEntry {
                    id: 0,
                    at_ms: epoch_ms(),
                    client: client.as_str().to_string(),
                    endpoint: endpoint.to_string(),
                    provider: String::new(),
                    upstream_protocol: String::new(),
                    requested_model: requested_model.to_string(),
                    upstream_model: String::new(),
                    converted: false,
                    streaming: false,
                    status: 0,
                    latency_ms: 0,
                    first_token_ms: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    error: None,
                    detail: None,
                    dropped: Vec::new(),
                    notes: Vec::new(),
                })),
                completed: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    pub fn handle(&self) -> TraceHandle {
        self.inner.as_ref().expect("trace inner").handle()
    }

    pub fn with_completed(mut self) -> (TraceHandle, StreamedTrace) {
        let inner = self.inner.take().expect("trace inner");
        let handle = inner.handle();
        (handle, inner)
    }

    pub fn complete(mut self, status: u16, error: Option<String>, detail: Option<String>) {
        if let Some(inner) = self.inner.take() {
            inner.complete(status, error, detail);
        }
    }

    pub fn error(&self, error: &crate::error::ProxyError) {
        self.handle().status(
            u16::from(error.status),
            Some(error.message.clone()),
            error.detail.clone(),
        );
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        drop(self.inner.take());
    }
}

pub struct StreamedTrace {
    bus: LogBus,
    _active: ActiveGuard,
    started: Instant,
    entry: Arc<Mutex<LogEntry>>,
    completed: Arc<AtomicBool>,
}

impl StreamedTrace {
    pub fn handle(&self) -> TraceHandle {
        TraceHandle {
            entry: Arc::clone(&self.entry),
            started: self.started,
        }
    }

    pub fn complete(self, status: u16, error: Option<String>, detail: Option<String>) {
        let started = self.started;
        {
            let mut entry = self.entry.lock().expect("trace lock");
            entry.status = status;
            if error.is_some() {
                entry.error = error;
            }
            entry.detail = detail.or(entry.detail.take());
            entry.latency_ms = elapsed(started);
        }
        self.completed.store(true, Ordering::Relaxed);
        self.bus
            .record(self.entry.lock().expect("trace lock").clone());
    }
}

impl Drop for StreamedTrace {
    fn drop(&mut self) {
        if self.completed.load(Ordering::Relaxed) {
            return;
        }
        self.completed.store(true, Ordering::Relaxed);
        let mut entry = self.entry.lock().expect("trace lock");
        entry.status = if entry.status == 0 { 499 } else { entry.status };
        entry.error = Some(
            entry
                .error
                .clone()
                .unwrap_or_else(|| "the request ended before a response was written".to_string()),
        );
        entry.latency_ms = elapsed(self.started);
        self.bus.record(entry.clone());
    }
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provider: &str, status: u16, latency_ms: u64) -> LogEntry {
        LogEntry {
            id: 0,
            at_ms: epoch_ms(),
            client: "openai".into(),
            endpoint: "/v1/chat/completions".into(),
            provider: provider.into(),
            upstream_protocol: "anthropic".into(),
            requested_model: "claude".into(),
            upstream_model: "claude".into(),
            converted: true,
            streaming: false,
            status,
            latency_ms,
            first_token_ms: None,
            input_tokens: 10,
            output_tokens: 5,
            error: None,
            detail: None,
            dropped: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn the_ring_buffer_keeps_the_newest_entries_and_numbers_them() {
        let bus = LogBus::new(16);
        for index in 0..20 {
            bus.record(entry("fast", if index % 5 == 0 { 500 } else { 200 }, 12));
        }

        let recent = bus.recent(3);
        assert_eq!(recent.len(), 3);
        assert!(recent[0].id > recent[1].id, "newest first");
        assert_eq!(bus.recent(1_000).len(), 16);

        let stats = bus.stats();
        assert_eq!(stats.requests, 20);
        assert_eq!(stats.errors, 4);
        assert_eq!(stats.input_tokens, 200);
        assert_eq!(stats.by_provider["fast"].avg_latency_ms, 12);
    }

    #[test]
    fn a_finished_trace_is_recorded_once_with_its_status() {
        let bus = LogBus::new(16);
        let trace = Trace::new(bus.clone(), Api::Chat, "/v1/chat/completions", "claude");
        let handle = trace.handle();
        handle.route("claude", "anthropic", "claude-sonnet-4-5", true);
        handle.tokens(11, 4);
        trace.complete(200, None, None);

        let recorded = bus.recent(1);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].status, 200);
        assert_eq!(recorded[0].provider, "claude".to_string());
        assert_eq!(recorded[0].output_tokens, 4);
    }

    #[test]
    fn a_dropped_stream_is_logged_as_cancelled() {
        let bus = LogBus::new(16);
        let trace = Trace::new(bus.clone(), Api::Messages, "/v1/messages", "gpt");
        let (_handle, streamed) = trace.with_completed();
        drop(streamed);

        let recorded = bus.recent(1);
        assert_eq!(recorded[0].status, 499);
        assert_eq!(
            recorded[0].error.as_deref(),
            Some("the request ended before a response was written")
        );
        assert_eq!(bus.stats().active, 0, "the in-flight counter is released");
    }

    #[test]
    fn completing_a_stream_does_not_double_record() {
        let bus = LogBus::new(16);
        let trace = Trace::new(bus.clone(), Api::Chat, "/v1/chat/completions", "gpt");
        let (_handle, streamed) = trace.with_completed();
        streamed.complete(200, None, None);

        assert_eq!(bus.stats().requests, 1);
    }

    #[test]
    fn subscribers_see_entries_as_they_land() {
        let bus = LogBus::new(16);
        let mut receiver = bus.subscribe();
        bus.record(entry("slow", 200, 30));

        let seen = receiver.try_recv().expect("a broadcast entry");
        assert_eq!(seen.provider, "slow".to_string());
    }
}
