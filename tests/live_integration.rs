/// Live integration tests — require a real OPENROUTER_API_KEY.
///
/// Run with:
///   cargo test --test live_integration -- --ignored --nocapture
///
/// All tests are marked `#[ignore]` so they are skipped in normal CI runs.
use std::sync::Arc;
use std::time::Duration;

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, ChatMessage, OpenRouterProvider, RequestConfig};
use coder_agent::tools;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Loads `.env` from the workspace root (silently ignored if absent), then
/// returns the provider.  Panics with a clear message if the key is still
/// missing so the test failure is obvious.
fn live_provider() -> Arc<OpenRouterProvider> {
    // Load .env relative to the crate root (CARGO_MANIFEST_DIR is set at
    // compile time and matches the working directory during `cargo test`).
    dotenvy::dotenv().ok();
    match OpenRouterProvider::from_env() {
        Some(p) => Arc::new(p),
        Option::None => panic!(
            "OPENROUTER_API_KEY is not set — add it to .env or export it before running live tests"
        ),
    }
}

/// Build a `RequestConfig` with all default codebase tools loaded.
fn live_config() -> RequestConfig {
    RequestConfig {
        tools: tools::default_tools(),
        ..RequestConfig::default()
    }
}

/// Drain the receiver until `Done` or `Error`, collecting every event.
/// Panics if no terminal event arrives within `timeout`.
fn collect_with_timeout(
    rx: std::sync::mpsc::Receiver<AgentEvent>,
    timeout: Duration,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out after {:?} waiting for Done/Error — events so far: {:#?}",
                timeout, events
            );
        }
        match rx.recv_timeout(remaining) {
            Ok(event) => {
                let terminal = matches!(event, AgentEvent::Done | AgentEvent::Error(_));
                events.push(event);
                if terminal {
                    return events;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!(
                    "timed out after {:?} waiting for Done/Error — events so far: {:#?}",
                    timeout, events
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Agent thread exited without sending Done — treat as done.
                return events;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Ask a question that requires no tools. Verifies the basic streaming pipeline
/// works against the real API.
#[test]
#[ignore = "requires OPENROUTER_API_KEY"]
fn test_live_plain_text_response() {
    let provider = live_provider();
    let config = live_config();

    let rx = agent_stream(
        provider,
        vec![ChatMessage::user(
            "Reply with exactly three words: one two three",
        )],
        config,
    );

    let events = collect_with_timeout(rx, Duration::from_secs(60));

    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "unexpected error: {:#?}",
        events.iter().find(|e| matches!(e, AgentEvent::Error(_)))
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Token(_))),
        "expected at least one Token event"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done"
    );
}

/// Ask the model to list the current directory.
///
/// Verifies the full tool-call round-trip against the real API:
///   1. Model calls `list_directory`  →  `ToolCall` event emitted
///   2. Tool executes locally         →  `ToolCallResult` event emitted
///   3. Model summarises the result   →  `Token` + `Done` events emitted
#[test]
#[ignore = "requires OPENROUTER_API_KEY"]
fn test_live_tool_call_list_directory() {
    let provider = live_provider();
    let config = live_config();

    let rx = agent_stream(
        provider,
        vec![ChatMessage::user(
            "Use the list_directory tool to list the current directory and tell me what you see.",
        )],
        config,
    );

    let events = collect_with_timeout(rx, Duration::from_secs(120));

    // Print all events for manual inspection when running with --nocapture
    for event in &events {
        match event {
            AgentEvent::Token(t) => print!("{}", t),
            AgentEvent::ToolCall(tc) => {
                println!(
                    "\n[ToolCall] name={} id={} args={}",
                    tc.name, tc.id, tc.arguments
                )
            }
            AgentEvent::ToolCallResult { info, output } => {
                println!("[ToolResult] {}  →  {} chars", info.name, output.len())
            }
            AgentEvent::Done => println!("\n[Done]"),
            AgentEvent::Error(e) => println!("\n[Error] {}", e),
            _ => {}
        }
    }

    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "unexpected error: {:#?}",
        events.iter().find(|e| matches!(e, AgentEvent::Error(_)))
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolCall(_))),
        "expected a ToolCall event — the model did not call any tool"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallResult { .. })),
        "expected a ToolCallResult event — tool was not executed"
    );

    // The tool result should contain at least one known file in this repo.
    let tool_output = events.iter().find_map(|e| {
        if let AgentEvent::ToolCallResult { output, .. } = e {
            Some(output.clone())
        } else {
            None
        }
    });
    let output = tool_output.expect("ToolCallResult must have an output");
    assert!(
        output.contains("Cargo.toml") || output.contains("src"),
        "expected directory listing to contain 'Cargo.toml' or 'src/', got: {}",
        output
    );

    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Done)),
        "expected Done"
    );
}
