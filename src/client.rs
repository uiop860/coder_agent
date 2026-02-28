use std::sync::mpsc::{Receiver, channel};

use futures::StreamExt;
use serde_json::{Value, json};

// ── Events emitted to the TUI ─────────────────────────────────────────────────

#[derive(Debug)]
pub enum AgentEvent {
    Token(String),
    ReasoningToken(String),
    Done,
    Error(String),
    /// The model issued a function tool call (emitted by the provider, consumed by the agent loop)
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// Emitted when a code block is about to be executed
    ScriptStarting,
    /// The return value from the executed script
    ScriptOutput(String),
    /// A JS/TS error from the executed script
    ScriptError(String),
}

// ── Shared message / config types ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    /// A tool result message — carries the id of the tool call it responds to.
    Tool { tool_call_id: String },
}

/// A single tool call made by the assistant (stored inside an assistant message).
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Tool calls requested by the assistant (only set for `ChatRole::Assistant` messages).
    pub tool_calls: Option<Vec<ToolCallInfo>>,
}

impl ChatMessage {
    #[allow(dead_code)]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool_calls: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_calls: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_calls: None,
        }
    }
    /// An assistant message that made one or more tool calls (content is usually empty).
    pub fn assistant_tool_call(tool_calls: Vec<ToolCallInfo>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: String::new(),
            tool_calls: Some(tool_calls),
        }
    }
    /// A tool result message sent back to the model after executing a tool call.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool {
                tool_call_id: tool_call_id.into(),
            },
            content: content.into(),
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestConfig {
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    /// "low" | "medium" | "high" — only sent if Some
    pub reasoning_effort: Option<String>,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            model: "nvidia/nemotron-3-nano-30b-a3b:free".to_string(),
            temperature: None,
            max_tokens: None,
            system_prompt: Some(
                "You are a code agent. You can execute TypeScript code to accomplish tasks.\n\
                 \n\
                 When you need to run code, use the `run_typescript` tool. Pass the TypeScript \
                 source as the `code` argument. The runtime will execute it and return the result.\n\
                 \n\
                 Available host functions inside the TypeScript runtime:\n\
                 - readFile(path: string): string — read a file from disk\n\
                 - writeFile(path: string, content: string): void — write a file to disk\n\
                 - print(...args: any[]): void — print to stderr for debugging\n\
                 \n\
                 The last expression (or a top-level `return` statement) becomes the return value.\n\
                 When you are done and have the final answer, respond with plain text (no tool call)."
                    .to_string(),
            ),
            reasoning_effort: None,
        }
    }
}

// ── Provider trait ────────────────────────────────────────────────────────────

pub trait Provider: Send + Sync {
    fn stream(&self, messages: Vec<ChatMessage>, config: &RequestConfig) -> Receiver<AgentEvent>;
}

// ── OpenRouter provider ───────────────────────────────────────────────────────

pub struct OpenRouterProvider {
    api_key: String,
    base_url: String,
}

impl OpenRouterProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
        }
    }

    pub fn from_env() -> Option<Self> {
        std::env::var("OPENROUTER_API_KEY").ok().map(Self::new)
    }
}

impl Provider for OpenRouterProvider {
    fn stream(&self, messages: Vec<ChatMessage>, config: &RequestConfig) -> Receiver<AgentEvent> {
        let (tx, rx) = channel();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let config = config.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async move {
                // ── Build input array (Responses API format) ──────────────
                let mut input: Vec<Value> = Vec::new();
                if let Some(ref sys) = config.system_prompt {
                    input.push(json!({ "type": "message", "role": "system", "content": sys }));
                }
                for msg in &messages {
                    match &msg.role {
                        ChatRole::System => {
                            input.push(json!({
                                "type": "message",
                                "role": "system",
                                "content": msg.content,
                            }));
                        }
                        ChatRole::User => {
                            input.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": msg.content,
                            }));
                        }
                        ChatRole::Assistant => {
                            if let Some(ref tcs) = msg.tool_calls {
                                // Flatten: one function_call item per tool call
                                for tc in tcs {
                                    input.push(json!({
                                        "type": "function_call",
                                        "call_id": tc.id,
                                        "name": tc.name,
                                        "arguments": tc.arguments,
                                    }));
                                }
                            } else {
                                input.push(json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": msg.content,
                                }));
                            }
                        }
                        ChatRole::Tool { tool_call_id } => {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_call_id,
                                "output": msg.content,
                            }));
                        }
                    }
                }

                // ── Tool definition (flat Responses API format) ───────────
                let tools = json!([{
                    "type": "function",
                    "name": "run_typescript",
                    "description": "Execute TypeScript code and return the result as a string.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "code": {
                                "type": "string",
                                "description": "TypeScript source code to execute. A top-level `return` statement sets the return value."
                            }
                        },
                        "required": ["code"]
                    }
                }]);

                // ── Build request body ────────────────────────────────────
                let mut body = json!({
                    "model": config.model,
                    "input": input,
                    "stream": true,
                    "tools": tools,
                    "tool_choice": "auto",
                });
                if let Some(t) = config.temperature {
                    body["temperature"] = json!(t);
                }
                if let Some(m) = config.max_tokens {
                    body["max_output_tokens"] = json!(m);
                }
                if let Some(ref effort) = config.reasoning_effort {
                    body["reasoning"] = json!({ "effort": effort });
                }

                let client = reqwest::Client::new();
                let resp = client
                    .post(format!("{}/responses", base_url))
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .header("HTTP-Referer", "https://github.com/coder_agent")
                    .header("X-Title", "coder_agent")
                    .json(&body)
                    .send()
                    .await;

                let response = match resp {
                    Err(e) => {
                        let _ = tx.send(AgentEvent::Error(e.to_string()));
                        return;
                    }
                    Ok(r) => r,
                };

                if !response.status().is_success() {
                    let status = response.status();
                    let body_text = response.text().await.unwrap_or_default();
                    let _ = tx.send(AgentEvent::Error(format!("HTTP {}: {}", status, body_text)));
                    return;
                }

                // ── Parse Responses API SSE stream ────────────────────────
                // Keyed by output_index: (call_id, name, accumulated_arguments)
                let mut tool_call_accum: std::collections::HashMap<usize, (String, String, String)> =
                    std::collections::HashMap::new();

                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();

                'outer: while let Some(chunk) = byte_stream.next().await {
                    match chunk {
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Error(e.to_string()));
                            return;
                        }
                        Ok(bytes) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));

                            while let Some(pos) = buffer.find('\n') {
                                let line = buffer[..pos].trim().to_string();
                                buffer.drain(..=pos);

                                if let Some(data) = line.strip_prefix("data: ") {
                                    if let Ok(val) = serde_json::from_str::<Value>(data) {
                                        let event_type = val["type"].as_str().unwrap_or("");
                                        match event_type {
                                            "response.output_item.added" => {
                                                if val["item"]["type"].as_str() == Some("function_call") {
                                                    let idx = val["output_index"].as_u64().unwrap_or(0) as usize;
                                                    let call_id = val["item"]["call_id"].as_str().unwrap_or("").to_string();
                                                    let name = val["item"]["name"].as_str().unwrap_or("").to_string();
                                                    tool_call_accum.insert(idx, (call_id, name, String::new()));
                                                }
                                            }
                                            "response.output_text.delta" => {
                                                if let Some(delta) = val["delta"].as_str() {
                                                    if !delta.is_empty() {
                                                        let _ = tx.send(AgentEvent::Token(delta.to_string()));
                                                    }
                                                }
                                            }
                                            "response.function_call_arguments.delta" => {
                                                let idx = val["output_index"].as_u64().unwrap_or(0) as usize;
                                                if let Some(delta) = val["delta"].as_str() {
                                                    if let Some(entry) = tool_call_accum.get_mut(&idx) {
                                                        entry.2.push_str(delta);
                                                    }
                                                }
                                            }
                                            "response.output_item.done" => {
                                                if val["item"]["type"].as_str() == Some("function_call") {
                                                    let idx = val["output_index"].as_u64().unwrap_or(0) as usize;
                                                    let call_id = val["item"]["call_id"].as_str().unwrap_or("").to_string();
                                                    let name = val["item"]["name"].as_str().unwrap_or("").to_string();
                                                    // Use item's complete arguments (authoritative)
                                                    let arguments = val["item"]["arguments"].as_str().unwrap_or("").to_string();
                                                    // Update or insert the complete data
                                                    tool_call_accum.insert(idx, (call_id, name, arguments));
                                                    // Emit the tool call now
                                                    if let Some((id, fn_name, args)) = tool_call_accum.remove(&idx) {
                                                        let _ = tx.send(AgentEvent::ToolCall {
                                                            id,
                                                            name: fn_name,
                                                            arguments: args,
                                                        });
                                                    }
                                                }
                                            }
                                            "response.completed" => {
                                                let _ = tx.send(AgentEvent::Done);
                                                break 'outer;
                                            }
                                            "response.failed" => {
                                                let err = val["response"]["error"]["message"]
                                                    .as_str()
                                                    .unwrap_or("unknown error")
                                                    .to_string();
                                                let _ = tx.send(AgentEvent::Error(err));
                                                break 'outer;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Stream ended
                let _ = tx.send(AgentEvent::Done);
            });
        });

        rx
    }
}
