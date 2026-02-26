mod common;

use std::sync::Arc;

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, ChatMessage, RequestConfig};
use coder_agent::executor::JsExecutorHandle;

use common::{MockProvider, collect_events, init_v8, tool_call_event};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal `RequestConfig` — disables reasoning so the mock responses are
/// never unexpectedly enriched.
fn cfg() -> RequestConfig {
    RequestConfig {
        reasoning_format: None,
        reasoning_effort: None,
        system_prompt: None,
        ..RequestConfig::default()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The LLM returns plain text with no TypeScript block.
/// The agent should forward the token(s) and emit `Done` without touching V8.
#[test]
fn test_simple_query_no_code() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    let provider = MockProvider::new(vec![vec![
        AgentEvent::Token("Here is your answer.".to_string()),
        AgentEvent::Done,
    ]]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
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
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptStarting)),
        "should not start a script for plain-text response"
    );
}

/// If the model issues a tool call we should see a corresponding
/// `AgentEvent::ToolCall` arrive on the stream; this ensures the TUI can
/// display it.  (The agent loop will still consume the event and act on it.)
#[test]
fn test_tool_call_forwarded_to_ui() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    let ts_code = "return 42;";
    let provider = MockProvider::new(vec![
        vec![tool_call_event("id-123", ts_code), AgentEvent::Done],
        vec![AgentEvent::Token("done".to_string()), AgentEvent::Done],
    ]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("test")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { .. })),
        "expected ToolCall event to be forwarded"
    );
}

/// When TypeScript writes a new file the sandbox should permit it, and a
/// subsequent read should return the correct content.  This exercised our
/// relaxed `ensure_within_base` logic.
#[test]
fn test_file_creation_and_readback() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    let ts_code = "
writeFile(\"foo_test.txt\", \"hi there\");
return readFile(\"foo_test.txt\");
";
    let provider = MockProvider::new(vec![vec![
        tool_call_event("file-1", ts_code),
        AgentEvent::Done,
    ]]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("create file")],
        cfg(),
    );
    let events = collect_events(rx);

    // ensure script output returned the expected string
    let output = events.iter().find_map(|e| {
        if let AgentEvent::ScriptOutput(s) = e {
            Some(s.as_str())
        } else {
            None
        }
    });
    assert_eq!(output, Some("hi there"));

    // cleanup the created file so tests are idempotent
    let _ = std::fs::remove_file("foo_test.txt");
}

/// The LLM calls `run_typescript` on the first turn.
/// V8 executes the code and the result is fed back; the LLM then responds
/// with plain text, ending the loop.
#[test]
fn test_single_tool_use_cycle() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    let ts_code = "const x: number = 2 + 2;\nreturn x;";
    let provider = MockProvider::new(vec![
        // First LLM turn: issue a run_typescript tool call
        vec![tool_call_event("call_001", ts_code), AgentEvent::Done],
        // Second LLM turn (after seeing script output): plain-text answer
        vec![
            AgentEvent::Token("The answer is 4.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("What is 2+2?")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptStarting)),
        "expected ScriptStarting"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptOutput(_))),
        "expected ScriptOutput"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done after second LLM turn"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptError(_))),
        "should be no script error for valid code"
    );

    // Confirm the script output contains the right value
    let output = events.iter().find_map(|e| {
        if let AgentEvent::ScriptOutput(s) = e {
            Some(s.as_str())
        } else {
            None
        }
    });
    assert_eq!(output, Some("4"), "V8 should return 4");
}

/// The LLM sends a TypeScript block that throws at runtime.
/// The agent should emit `ScriptError`, feed the error back to the LLM, and
/// continue — ultimately terminating when the LLM returns plain text.
#[test]
fn test_script_runtime_error() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    // This code throws; V8 will return a ScriptError
    let ts_code = "throw new Error('deliberate failure');";
    let provider = MockProvider::new(vec![
        vec![tool_call_event("call_err", ts_code), AgentEvent::Done],
        // LLM acknowledges the error and responds with plain text
        vec![
            AgentEvent::Token("I see the error occurred.".to_string()),
            AgentEvent::Done,
        ],
    ]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("Run something that errors")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptStarting)),
        "expected ScriptStarting even for a failing script"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptError(_))),
        "expected ScriptError from the thrown exception"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done after the LLM responds with plain text"
    );
}

/// When the LLM keeps emitting TypeScript blocks the agent stops after
/// MAX_ITERATIONS (10) to break infinite loops.
/// The loop must always terminate and the final events must include Done.
#[test]
fn test_max_iterations_respected() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    // Provide 12 responses — all with a tool call — which is more than
    // MAX_ITERATIONS = 10.  The agent must stop at iteration 10.
    let ts_code = "return 1;";
    let responses: Vec<Vec<AgentEvent>> = (0..12)
        .map(|i| {
            vec![
                tool_call_event(&format!("call_{i}"), ts_code),
                AgentEvent::Done,
            ]
        })
        .collect();

    let provider = MockProvider::new(responses);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("Loop forever")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "agent must eventually send Done"
    );

    // The limit-hit ScriptError should be present
    let has_limit_error = events.iter().any(|e| {
        if let AgentEvent::ScriptError(msg) = e {
            msg.contains("Maximum iteration limit")
        } else {
            false
        }
    });
    assert!(
        has_limit_error,
        "expected a 'Maximum iteration limit' ScriptError"
    );
}

/// When the provider itself emits an `Error` event the agent terminates and
/// propagates the error.
#[test]
fn test_provider_error_terminates() {
    init_v8();
    let executor = JsExecutorHandle::spawn();

    let provider = MockProvider::new(vec![vec![AgentEvent::Error(
        "API error: unauthorized".to_string(),
    )]]);

    let rx = agent_stream(
        Arc::new(provider),
        executor,
        vec![ChatMessage::user("Anything")],
        cfg(),
    );
    let events = collect_events(rx);

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "provider error must be forwarded to the caller"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScriptStarting)),
        "no code should run when the provider errors immediately"
    );
}
