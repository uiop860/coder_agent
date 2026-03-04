mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
        cancel,
        approval_rx,
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

// ── Retry tests ───────────────────────────────────────────────────────────────

/// A 429 rate-limit error on the first attempt should be retried.
/// When the second attempt succeeds the agent emits `Done` and a
/// "Retrying" status token should appear before the final answer.
///
/// Note: incurs a 1 s backoff sleep (2^0 = 1 s).
#[test]
fn test_retry_on_rate_limit_then_success() {
    let provider = MockProvider::new(vec![
        vec![AgentEvent::Error(
            "HTTP 429: rate limit exceeded".to_string(),
        )],
        vec![
            AgentEvent::Token("Answer after retry.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
        cancel,
        approval_rx,
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done — agent should succeed after retry"
    );
    assert!(
        events.iter().any(|e| {
            if let AgentEvent::Token(t) = e {
                t.contains("Retrying")
            } else {
                false
            }
        }),
        "expected a Retrying status token to be emitted before the retry"
    );
}

/// A 503 server error on the first attempt should be retried.
/// Mirrors the 429 test but verifies 5xx codes are also classified as retryable.
///
/// Note: incurs a 1 s backoff sleep.
#[test]
fn test_retry_on_server_error_then_success() {
    let provider = MockProvider::new(vec![
        vec![AgentEvent::Error(
            "HTTP 503: service unavailable".to_string(),
        )],
        vec![
            AgentEvent::Token("Answer after retry.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
        cancel,
        approval_rx,
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done — agent should succeed after retry on 503"
    );
}

/// A 4xx error other than 429 (e.g. 401 Unauthorized) is not transient and
/// must not be retried.  The MockProvider is programmed with a fallback `Done`
/// for the second call: if the agent incorrectly retries we would see `Done`
/// instead of `Error`.
#[test]
fn test_non_retryable_4xx_not_retried() {
    let provider = MockProvider::new(vec![
        vec![AgentEvent::Error("HTTP 401: unauthorized".to_string())],
        // fallback — must never be reached
        vec![AgentEvent::Done],
    ]);

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
        cancel,
        approval_rx,
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "expected Error — 401 must not be retried"
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "must not reach Done — that would mean the agent incorrectly retried"
    );
}

/// An error that occurs *after* tokens have been emitted (mid-stream) must not
/// be retried, because the partial response is already visible in the TUI.
/// The fallback second response is `Done`; seeing it would mean a retry occurred.
#[test]
fn test_mid_stream_error_not_retried() {
    let provider = MockProvider::new(vec![
        vec![
            AgentEvent::Token("partial answer...".to_string()),
            AgentEvent::Error("HTTP 503: dropped mid-stream".to_string()),
        ],
        // fallback — must never be reached
        vec![AgentEvent::Done],
    ]);

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Hello")],
        cfg(),
        cancel,
        approval_rx,
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "expected Error — mid-stream error must propagate"
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "must not reach Done — that would mean a mid-stream error was incorrectly retried"
    );
}

/// When the provider itself emits an `Error` event the agent terminates and
/// propagates the error.
#[test]
fn test_provider_error_terminates() {
    let provider = MockProvider::new(vec![vec![AgentEvent::Error(
        "HTTP 401: unauthorized".to_string(),
    )]]);

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Anything")],
        cfg(),
        cancel,
        approval_rx,
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
        vec![AgentEvent::ToolCall(tool_call.clone()), AgentEvent::Done],
        // Second pass: plain text response after tool result
        vec![
            AgentEvent::Token("The file contains package metadata.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let mut config = cfg();
    config.tools = coder_agent::tools::default_tools();

    let cancel = Arc::new(AtomicBool::new(false));
    let (_, approval_rx) = std::sync::mpsc::channel::<bool>();
    let rx = agent_stream(
        Arc::new(provider),
        vec![ChatMessage::user("Read the project manifest file")],
        config,
        cancel,
        approval_rx,
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
