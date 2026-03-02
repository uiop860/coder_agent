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

/// A single tool use cycle: agent requests a tool call, we execute it and send
/// back the result, and the agent emits the tool call and result events.
#[test]
fn test_single_tool_use_cycle() {
    use coder_agent::client::ToolCallInfo;

    // First pass: LLM returns a tool call
    // Second pass: LLM sees the tool result and returns plain text
    let tool_call = ToolCallInfo {
        id: "call_123".to_string(),
        name: "read_file".to_string(),
        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
    };

    let provider = MockProvider::new(vec![
        // First pass: tool call
        vec![
            AgentEvent::ToolCall(tool_call.clone()),
            AgentEvent::Done,
        ],
        // Second pass: plain text response after tool result
        vec![
            AgentEvent::Token("The file contains package metadata.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let mut config = cfg();
    config.tools = coder_agent::tools::default_tools();

    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Read the project manifest file")],
        config,
    );
    let events = collect_events(rx);

    // Verify the expected event sequence
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolCall(_))),
        "expected ToolCall event"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallResult { .. })),
        "expected ToolCallResult event showing tool was executed"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Token(_))),
        "expected Token event from second LLM pass"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done event"
    );
}
