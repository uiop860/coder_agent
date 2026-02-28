use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::client::{AgentEvent, ChatMessage, Provider, RequestConfig};

/// Run one LLM streaming pass and emit tokens to the TUI.

/// Run one LLM streaming pass.
///
/// Content tokens / reasoning tokens are forwarded straight to the TUI via `tx`.
fn run_one_llm_pass(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    config: &RequestConfig,
    tx: &Sender<AgentEvent>,
) -> Result<(), String> {
    let rx = provider.stream(messages.to_vec(), config);

    loop {
        match rx.recv() {
            Ok(AgentEvent::Token(t)) => {
                let _ = tx.send(AgentEvent::Token(t));
            }
            Ok(AgentEvent::ReasoningToken(t)) => {
                let _ = tx.send(AgentEvent::ReasoningToken(t));
            }
            Ok(AgentEvent::Done) => {
                return Ok(());
            }
            Ok(AgentEvent::Error(e)) => {
                let _ = tx.send(AgentEvent::Error(e.clone()));
                return Err(e);
            }
            Err(_) => {
                return Err("provider channel closed unexpectedly".to_string());
            }
        }
    }
}

/// Start an LLM streaming pass on a new OS thread.
///
/// This is the main entry point the TUI uses. Streams the LLM's response
/// tokens and reasoning to the caller until Done or Error.
pub fn agent_stream(
    provider: Arc<dyn Provider>,
    initial_messages: Vec<ChatMessage>,
    config: RequestConfig,
) -> Receiver<AgentEvent> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        match run_one_llm_pass(&*provider, &initial_messages, &config, &tx) {
            Ok(()) => {
                let _ = tx.send(AgentEvent::Done);
            }
            Err(_) => {
                // error already forwarded to TUI by run_one_llm_pass
            }
        }
    });

    rx
}
