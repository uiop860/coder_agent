use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use log::{debug, error, info, warn};

use crate::client::{AgentEvent, ChatMessage, Provider, RequestConfig, ToolCallInfo};

const MAX_ITERATIONS: usize = 10;
const MAX_RETRIES: u32 = 3;

enum PassError {
    Cancelled,
    /// Transient failure before any tokens were emitted — safe to retry.
    Retryable(String),
    /// Non-retryable failure, or error that occurred after tokens were emitted.
    Fatal(String),
}

/// Returns true for errors that are worth retrying: rate limits, server errors,
/// and network-level failures.  Client errors (4xx except 429) are not retried.
fn is_retryable_error(e: &str) -> bool {
    if let Some(rest) = e.strip_prefix("HTTP ") {
        // "HTTP 429 …" → rate limited
        // "HTTP 5xx …" → server error
        rest.starts_with("429") || rest.starts_with('5')
    } else {
        // reqwest network errors, channel-closed, etc.
        true
    }
}

/// One attempt at an LLM streaming pass.  Does NOT retry on its own.
/// Classifies errors as Retryable only when no tokens have been forwarded yet
/// (so the TUI has not started rendering a partial response).
fn try_llm_pass(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    config: &RequestConfig,
    tx: &Sender<AgentEvent>,
    cancel: &AtomicBool,
) -> Result<(Vec<ToolCallInfo>, String), PassError> {
    let rx = provider.stream(messages.to_vec(), config);
    let mut tool_calls: Vec<ToolCallInfo> = Vec::new();
    let mut text = String::new();
    let mut tokens_emitted = false;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(PassError::Cancelled);
        }
        match rx.recv() {
            Ok(AgentEvent::Token(t)) => {
                tokens_emitted = true;
                text.push_str(&t);
                let _ = tx.send(AgentEvent::Token(t));
            }
            Ok(AgentEvent::ReasoningToken(t)) => {
                tokens_emitted = true;
                let _ = tx.send(AgentEvent::ReasoningToken(t));
            }
            Ok(AgentEvent::ToolCall(tc)) => {
                info!(
                    "try_llm_pass: tool call received — name={} id={} args={}",
                    tc.name, tc.id, tc.arguments
                );
                let _ = tx.send(AgentEvent::ToolCall(tc.clone()));
                tool_calls.push(tc);
            }
            Ok(AgentEvent::Done) => {
                debug!(
                    "try_llm_pass: Done — {} tool call(s), {} text chars",
                    tool_calls.len(),
                    text.len()
                );
                return Ok((tool_calls, text));
            }
            Ok(AgentEvent::Error(e)) => {
                error!("try_llm_pass: provider error — {}", e);
                if !tokens_emitted && is_retryable_error(&e) {
                    return Err(PassError::Retryable(e));
                }
                return Err(PassError::Fatal(e));
            }
            Ok(AgentEvent::ToolCallResult { .. }) | Ok(AgentEvent::ToolApprovalRequest { .. }) => {}
            Ok(AgentEvent::Usage {
                input_tokens,
                output_tokens,
            }) => {
                let _ = tx.send(AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                });
            }
            Err(_) => {
                error!("try_llm_pass: provider channel closed unexpectedly");
                let e = "provider channel closed unexpectedly".to_string();
                if !tokens_emitted {
                    return Err(PassError::Retryable(e));
                }
                return Err(PassError::Fatal(e));
            }
        }
    }
}

/// Run one LLM streaming pass, retrying up to MAX_RETRIES times on transient
/// errors with exponential backoff (1 s → 2 s → 4 s).
fn run_one_llm_pass(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    config: &RequestConfig,
    tx: &Sender<AgentEvent>,
    cancel: &AtomicBool,
) -> Result<(Vec<ToolCallInfo>, String), String> {
    debug!(
        "run_one_llm_pass: starting with {} messages",
        messages.len()
    );

    for attempt in 0..=MAX_RETRIES {
        match try_llm_pass(provider, messages, config, tx, cancel) {
            Ok(result) => return Ok(result),
            Err(PassError::Cancelled) => return Err("cancelled".to_string()),
            Err(PassError::Retryable(e)) if attempt < MAX_RETRIES => {
                let delay = Duration::from_secs(2u64.pow(attempt));
                warn!(
                    "run_one_llm_pass: retryable error (attempt {}/{}) in {}s — {}",
                    attempt + 1,
                    MAX_RETRIES,
                    delay.as_secs(),
                    e
                );
                let _ = tx.send(AgentEvent::Token(format!(
                    "\n*(Retrying in {}s — {})*\n",
                    delay.as_secs(),
                    e
                )));
                std::thread::sleep(delay);
            }
            Err(PassError::Retryable(e)) | Err(PassError::Fatal(e)) => {
                error!("run_one_llm_pass: giving up — {}", e);
                let _ = tx.send(AgentEvent::Error(e.clone()));
                return Err(e);
            }
        }
    }

    unreachable!()
}

/// Start an agentic LLM loop on a new OS thread.
///
/// This is the main entry point the TUI uses. Implements a loop that handles
/// tool calls: it streams tokens/tool calls to the caller, executes tools,
/// and continues the conversation with tool results.
pub fn agent_stream(
    provider: Arc<dyn Provider>,
    initial_messages: Vec<ChatMessage>,
    config: RequestConfig,
    cancel: Arc<AtomicBool>,
    approval_rx: std::sync::mpsc::Receiver<bool>,
) -> Receiver<AgentEvent> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        info!(
            "agent_stream: starting — {} initial message(s), {} tool(s) registered",
            initial_messages.len(),
            config.tools.len()
        );

        let mut messages = initial_messages;
        for iteration in 0..MAX_ITERATIONS {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(AgentEvent::Done);
                return;
            }

            debug!(
                "agent_stream: iteration {} / {}",
                iteration + 1,
                MAX_ITERATIONS
            );

            let (tool_calls, _text) =
                match run_one_llm_pass(&*provider, &messages, &config, &tx, &cancel) {
                    Ok(r) => r,
                    Err(e) if e == "cancelled" => {
                        let _ = tx.send(AgentEvent::Done);
                        return;
                    }
                    Err(e) => {
                        error!("agent_stream: LLM pass failed — {}", e);
                        return;
                    }
                };

            if tool_calls.is_empty() {
                info!("agent_stream: no tool calls — sending Done");
                let _ = tx.send(AgentEvent::Done);
                return;
            }

            info!("agent_stream: {} tool call(s) to execute", tool_calls.len());

            // Append the assistant's tool-call message
            messages.push(ChatMessage::assistant_tool_call(tool_calls.clone()));

            // Execute each tool and append results
            for tc in &tool_calls {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(AgentEvent::Done);
                    return;
                }

                info!(
                    "agent_stream: executing tool '{}' (id={}) args={}",
                    tc.name, tc.id, tc.arguments
                );

                // Check if this tool requires approval
                let needs_approval = config
                    .tools
                    .iter()
                    .find(|t| t.definition().name == tc.name)
                    .map(|t| t.requires_approval())
                    .unwrap_or(false);

                if needs_approval {
                    let _ = tx.send(AgentEvent::ToolApprovalRequest { info: tc.clone() });
                    match approval_rx.recv() {
                        Ok(true) => { /* approved, proceed */ }
                        Ok(false) | Err(_) => {
                            info!("agent_stream: tool '{}' denied by user", tc.name);
                            let _ = tx.send(AgentEvent::ToolCallResult {
                                info: tc.clone(),
                                output: "Tool execution denied by user.".to_string(),
                            });
                            messages.push(ChatMessage::tool_result(
                                &tc.id,
                                "Tool execution denied by user.",
                            ));
                            continue;
                        }
                    }
                }

                let result = config
                    .tools
                    .iter()
                    .find(|t| t.definition().name == tc.name)
                    .map(|t| {
                        t.execute(&tc.arguments).unwrap_or_else(|e| {
                            warn!("agent_stream: tool '{}' returned error — {}", tc.name, e);
                            format!("Error: {}", e)
                        })
                    })
                    .unwrap_or_else(|| {
                        warn!("agent_stream: unknown tool '{}'", tc.name);
                        format!("Unknown tool: {}", tc.name)
                    });

                debug!(
                    "agent_stream: tool '{}' result ({} chars):\n{}",
                    tc.name,
                    result.len(),
                    result
                );

                let _ = tx.send(AgentEvent::ToolCallResult {
                    info: tc.clone(),
                    output: result.clone(),
                });
                messages.push(ChatMessage::tool_result(&tc.id, result));
            }
        }

        error!(
            "agent_stream: maximum iterations ({}) reached",
            MAX_ITERATIONS
        );
        let _ = tx.send(AgentEvent::Error("Maximum iterations reached".to_string()));
    });

    rx
}
