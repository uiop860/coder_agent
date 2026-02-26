use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};

use coder_agent::client::{AgentEvent, ChatMessage, Provider, RequestConfig};

// ── V8 one-time initialisation ────────────────────────────────────────────────

static V8_INIT: OnceLock<()> = OnceLock::new();

/// Call this at the top of every test that creates a `JsExecutorHandle`.
/// Safe to call from multiple threads; V8 is initialised exactly once.
pub fn init_v8() {
    V8_INIT.get_or_init(|| {
        let platform = v8::new_unprotected_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

// ── Mock LLM provider ─────────────────────────────────────────────────────────

/// A `Provider` that replays pre-programmed sequences of `AgentEvent`s.
///
/// Each [`Vec<AgentEvent>`] in `responses` corresponds to one `stream()` call.
/// If the provider is called more times than there are responses, it falls back
/// to a single `Done` event.
pub struct MockProvider {
    responses: Arc<Mutex<VecDeque<Vec<AgentEvent>>>>,
}

impl MockProvider {
    pub fn new(responses: Vec<Vec<AgentEvent>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }
}

impl Provider for MockProvider {
    fn stream(&self, _messages: Vec<ChatMessage>, _config: &RequestConfig) -> Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel();

        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![AgentEvent::Done]);

        std::thread::spawn(move || {
            for event in events {
                let _ = tx.send(event);
            }
        });

        rx
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Build a `run_typescript` tool-call event as the mock LLM would emit.
/// `call_id` should be unique per test to match the tool-result reply.
pub fn tool_call_event(call_id: &str, ts_code: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: call_id.to_string(),
        name: "run_typescript".to_string(),
        arguments: serde_json::json!({ "code": ts_code }).to_string(),
    }
}

/// Drain a `agent_stream` receiver into a `Vec`, stopping after `Done` or
/// `Error`, or when the channel closes (agent thread exited).
pub fn collect_events(rx: Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        match rx.recv() {
            Ok(event) => {
                let is_terminal = matches!(event, AgentEvent::Done | AgentEvent::Error(_));
                events.push(event);
                if is_terminal {
                    break;
                }
            }
            Err(_) => break, // channel closed — agent thread exited
        }
    }
    events
}
