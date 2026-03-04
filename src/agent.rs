use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use log::{debug, error, info, warn};

use crate::client::{AgentEvent, ChatMessage, Provider, RequestConfig, ToolCallInfo};

const MAX_ITERATIONS: usize = 10;

/// Run one LLM streaming pass.
///
/// Content tokens / reasoning tokens are forwarded straight to the TUI via `tx`.
/// Returns the list of tool calls and any text generated.
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
    let rx = provider.stream(messages.to_vec(), config);
    let mut tool_calls: Vec<ToolCallInfo> = Vec::new();
    let mut text = String::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        match rx.recv() {
            Ok(AgentEvent::Token(t)) => {
                text.push_str(&t);
                let _ = tx.send(AgentEvent::Token(t));
            }
            Ok(AgentEvent::ReasoningToken(t)) => {
                let _ = tx.send(AgentEvent::ReasoningToken(t));
            }
            Ok(AgentEvent::ToolCall(tc)) => {
                info!(
                    "run_one_llm_pass: tool call received — name={} id={} args={}",
                    tc.name, tc.id, tc.arguments
                );
                let _ = tx.send(AgentEvent::ToolCall(tc.clone()));
                tool_calls.push(tc);
            }
            Ok(AgentEvent::Done) => {
                debug!(
                    "run_one_llm_pass: Done — {} tool call(s), {} text chars",
                    tool_calls.len(),
                    text.len()
                );
                return Ok((tool_calls, text));
            }
            Ok(AgentEvent::Error(e)) => {
                error!("run_one_llm_pass: provider error — {}", e);
                let _ = tx.send(AgentEvent::Error(e.clone()));
                return Err(e);
            }
            Ok(AgentEvent::ToolCallResult { .. }) | Ok(AgentEvent::ToolApprovalRequest { .. }) => {
                // Should not come from provider; ignore.
            }
            Ok(AgentEvent::Usage {
                input_tokens,
                output_tokens,
            }) => {
                // Forward usage stats to the TUI.
                let _ = tx.send(AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                });
            }
            Err(_) => {
                error!("run_one_llm_pass: provider channel closed unexpectedly");
                return Err("provider channel closed unexpectedly".to_string());
            }
        }
    }
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
