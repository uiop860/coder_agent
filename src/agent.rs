use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::client::{AgentEvent, ChatMessage, Provider, RequestConfig, ToolCallInfo};
use crate::executor::JsExecutorHandle;

const MAX_ITERATIONS: usize = 10;

/// The two ways an LLM streaming pass can end.
enum LlmPassResult {
    /// The model produced a text reply — we're done with this turn.
    /// Tokens were already forwarded to the TUI; the accumulated string is
    /// kept here for symmetry but the agent loop does not need to re-read it.
    Text(#[allow(dead_code)] String),
    /// The model issued a `run_typescript` tool call.
    ToolCall {
        call_id: String,
        arguments: String, // raw JSON, e.g. `{"code":"return 1+1;"}`
    },
}

/// Run one LLM streaming pass.
///
/// Content tokens / reasoning tokens are forwarded straight to the TUI via
/// `tx`.  Returns `LlmPassResult::Text` for plain responses, or
/// `LlmPassResult::ToolCall` when the model invokes `run_typescript`.
fn run_one_llm_pass(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    config: &RequestConfig,
    tx: &Sender<AgentEvent>,
) -> Result<LlmPassResult, String> {
    let rx = provider.stream(messages.to_vec(), config);
    let mut content = String::new();
    let mut pending_tool_call: Option<(String, String)> = None; // (call_id, arguments)

    loop {
        match rx.recv() {
            Ok(AgentEvent::Token(t)) => {
                content.push_str(&t);
                let _ = tx.send(AgentEvent::Token(t));
            }
            Ok(AgentEvent::ReasoningToken(t)) => {
                let _ = tx.send(AgentEvent::ReasoningToken(t));
            }
            Ok(AgentEvent::ToolCall {
                id,
                name,
                arguments,
            }) => {
                // Forward to the UI so the user can inspect the call.
                let _ = tx.send(AgentEvent::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });

                // We only handle our single registered tool.
                if name == "run_typescript" {
                    pending_tool_call = Some((id, arguments));
                } else {
                    // Unknown tool — surface as error
                    let msg = format!("Unknown tool call: {name}");
                    let _ = tx.send(AgentEvent::Error(msg.clone()));
                    return Err(msg);
                }
            }
            Ok(AgentEvent::Done) => {
                return Ok(match pending_tool_call {
                    Some((call_id, arguments)) => LlmPassResult::ToolCall { call_id, arguments },
                    None => LlmPassResult::Text(content),
                });
            }
            Ok(AgentEvent::Error(e)) => {
                let _ = tx.send(AgentEvent::Error(e.clone()));
                return Err(e);
            }
            // ScriptStarting / ScriptOutput / ScriptError come from us, not the provider
            Ok(other) => {
                let _ = tx.send(other);
            }
            Err(_) => {
                return Err("provider channel closed unexpectedly".to_string());
            }
        }
    }
}

/// Start the agentic loop on a new OS thread.
///
/// This is the main entry point the TUI uses.
pub fn agent_stream(
    provider: Arc<dyn Provider>,
    executor: JsExecutorHandle,
    initial_messages: Vec<ChatMessage>,
    config: RequestConfig,
) -> Receiver<AgentEvent> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut messages = initial_messages;
        let mut iteration = 0;

        loop {
            iteration += 1;

            if iteration > MAX_ITERATIONS {
                let _ = tx.send(AgentEvent::ScriptError(
                    "Maximum iteration limit reached".to_string(),
                ));
                let _ = tx.send(AgentEvent::Done);
                break;
            }

            let pass = match run_one_llm_pass(&*provider, &messages, &config, &tx) {
                Ok(p) => p,
                Err(_) => break, // error already forwarded to TUI
            };

            match pass {
                LlmPassResult::Text(_) => {
                    // Plain text response — nothing more to do.
                    let _ = tx.send(AgentEvent::Done);
                    break;
                }
                LlmPassResult::ToolCall { call_id, arguments } => {
                    // Parse the "code" field out of the JSON arguments.
                    let code = match serde_json::from_str::<serde_json::Value>(&arguments)
                        .ok()
                        .and_then(|v| v["code"].as_str().map(|s| s.to_string()))
                    {
                        Some(c) => c,
                        None => {
                            let err = format!(
                                "run_typescript: could not parse 'code' from arguments: {arguments}"
                            );
                            let _ = tx.send(AgentEvent::ScriptError(err.clone()));
                            // Tell the LLM what went wrong so it can retry.
                            messages.push(ChatMessage::assistant_tool_call(vec![ToolCallInfo {
                                id: call_id.clone(),
                                name: "run_typescript".to_string(),
                                arguments,
                            }]));
                            messages.push(ChatMessage::tool_result(&call_id, err));
                            continue;
                        }
                    };

                    let _ = tx.send(AgentEvent::ScriptStarting);

                    let tool_result = match executor.run_ts(&code) {
                        Ok(output) => {
                            let _ = tx.send(AgentEvent::ScriptOutput(output.clone()));
                            output
                        }
                        Err(err) => {
                            let _ = tx.send(AgentEvent::ScriptError(err.clone()));
                            format!("Error: {err}")
                        }
                    };

                    // Record the assistant's tool call and our result in the history
                    // so the model can see both on the next turn.
                    messages.push(ChatMessage::assistant_tool_call(vec![ToolCallInfo {
                        id: call_id.clone(),
                        name: "run_typescript".to_string(),
                        arguments: serde_json::json!({ "code": code }).to_string(),
                    }]));
                    messages.push(ChatMessage::tool_result(&call_id, tool_result));
                    // Loop — the LLM will decide whether to run more code or respond.
                }
            }
        }
    });

    rx
}
