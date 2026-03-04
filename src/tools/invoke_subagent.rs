use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{Tool, ToolDefinition};
use crate::agent::agent_stream;
use crate::agents;
use crate::client::{AgentEvent, ChatMessage, Provider, RequestConfig, ToolCallInfo};

pub struct InvokeSubagentTool {
    provider: Arc<dyn Provider>,
    /// Shared slot for forwarding sub-agent events to the TUI in real time.
    /// The App sets a fresh Sender before each streaming session.
    progress_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<AgentEvent>>>>,
}

impl InvokeSubagentTool {
    pub fn new(
        provider: Arc<dyn Provider>,
        progress_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<AgentEvent>>>>,
    ) -> Self {
        Self {
            provider,
            progress_tx,
        }
    }
}

impl Tool for InvokeSubagentTool {
    fn definition(&self) -> ToolDefinition {
        let profile_names: Vec<&str> = agents::AGENT_PROFILES.iter().map(|p| p.name).collect();
        ToolDefinition {
            name: "invoke_subagent",
            description: "Delegate a task to a specialised sub-agent. \
The sub-agent runs to completion and its final response is returned as the tool output.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Name of the sub-agent profile to use.",
                        "enum": profile_names
                    },
                    "task": {
                        "type": "string",
                        "description": "The task or question to send to the sub-agent."
                    }
                },
                "required": ["agent", "task"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let agent_name = args["agent"]
            .as_str()
            .ok_or("Missing 'agent' field")?
            .to_string();
        let task = args["task"].as_str().ok_or("Missing 'task' field")?;

        let profile = agents::get_profile(&agent_name)
            .ok_or_else(|| format!("Unknown agent profile: '{}'", agent_name))?;

        let config = RequestConfig {
            system_prompt: Some(profile.system_prompt.to_string()),
            tools: (profile.make_tools)(),
            ..RequestConfig::default()
        };

        let cancel = Arc::new(AtomicBool::new(false));
        // Drop sender immediately so sub-agent approval requests are auto-denied.
        let (_approval_tx, approval_rx) = std::sync::mpsc::channel::<bool>();
        drop(_approval_tx);

        let rx = agent_stream(
            self.provider.clone(),
            vec![ChatMessage::user(task)],
            config,
            cancel,
            approval_rx,
        );

        let mut result = String::new();
        loop {
            match rx.recv() {
                Ok(AgentEvent::Token(t)) => result.push_str(&t),
                Ok(AgentEvent::ToolCall(tc)) => {
                    let prefixed = ToolCallInfo {
                        id: tc.id.clone(),
                        name: format!("[{}] {}", agent_name, tc.name),
                        arguments: tc.arguments.clone(),
                    };
                    if let Ok(guard) = self.progress_tx.lock()
                        && let Some(tx) = guard.as_ref() {
                            let _ = tx.send(AgentEvent::ToolCall(prefixed));
                        }
                }
                Ok(AgentEvent::ToolCallResult { info, output }) => {
                    let prefixed_info = ToolCallInfo {
                        id: info.id.clone(),
                        name: format!("[{}] {}", agent_name, info.name),
                        arguments: info.arguments.clone(),
                    };
                    if let Ok(guard) = self.progress_tx.lock()
                        && let Some(tx) = guard.as_ref() {
                            let _ = tx.send(AgentEvent::ToolCallResult {
                                info: prefixed_info,
                                output,
                            });
                        }
                }
                Ok(AgentEvent::Done) => break,
                Ok(AgentEvent::Error(e)) => return Err(format!("Sub-agent error: {}", e)),
                Err(_) => break,
                _ => {}
            }
        }

        Ok(result)
    }
}
