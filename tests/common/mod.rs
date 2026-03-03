use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use coder_agent::client::{AgentEvent, ChatMessage, Provider, RequestConfig};

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

/// Drain a `agent_stream` receiver into a `Vec`, stopping after `Done` or
/// `Error`, or when the channel closes (agent thread exited).
pub fn collect_events(rx: Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.recv() {
        let is_terminal = matches!(event, AgentEvent::Done | AgentEvent::Error(_));
        events.push(event);
        if is_terminal {
            break;
        }
    }
    events
}
