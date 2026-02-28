mod common;

use std::sync::Arc;

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, ChatMessage, RequestConfig};

use common::{MockProvider, collect_events};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal `RequestConfig` — disables reasoning so the mock responses are
/// never unexpectedly enriched.
fn cfg() -> RequestConfig {
    RequestConfig {
        reasoning_effort: None,
        system_prompt: None,
        ..RequestConfig::default()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The LLM returns plain text.
/// The agent should forward the token(s) and emit `Done`.
#[test]
fn test_simple_query_no_code() {
    let provider = MockProvider::new(vec![vec![
        AgentEvent::Token("Here is your answer.".to_string()),
        AgentEvent::Done,
    ]]);

    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Token(_))),
        "expected at least one Token event"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done"
    );
}

/// When the provider itself emits an `Error` event the agent terminates and
/// propagates the error.
#[test]
fn test_provider_error_terminates() {
    let provider = MockProvider::new(vec![vec![AgentEvent::Error(
        "API error: unauthorized".to_string(),
    )]]);

    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Anything")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "provider error must be forwarded to the caller"
    );
}
