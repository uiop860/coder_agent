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
    /// "parsed" | "raw" | "hidden" | "none"
    pub reasoning_format: Option<String>,
    /// "low" | "medium" | "high" (gpt-oss-120b only)
    pub reasoning_effort: Option<String>,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            model: "gpt-oss-120b".to_string(),
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
            reasoning_format: Some("parsed".to_string()),
            reasoning_effort: Some("medium".to_string()),
        }
    }
}

// ── Provider trait ────────────────────────────────────────────────────────────

pub trait Provider: Send + Sync {
    fn stream(&self, messages: Vec<ChatMessage>, config: &RequestConfig) -> Receiver<AgentEvent>;
}

// ── Cerebras provider ─────────────────────────────────────────────────────────

pub struct CerebrasProvider {
    api_key: String,
    base_url: String,
}

impl CerebrasProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.cerebras.ai/v1".to_string(),
        }
    }

    pub fn from_env() -> Option<Self> {
        std::env::var("CEREBRAS_API_KEY").ok().map(Self::new)
    }
}

impl Provider for CerebrasProvider {
    fn stream(&self, messages: Vec<ChatMessage>, config: &RequestConfig) -> Receiver<AgentEvent> {
        let (tx, rx) = channel();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let config = config.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async move {
                // ── Build messages array ──────────────────────────────────
                let mut msgs: Vec<Value> = Vec::new();
                if let Some(ref sys) = config.system_prompt {
                    msgs.push(json!({ "role": "system", "content": sys }));
                }
                for msg in &messages {
                    let json_msg = match &msg.role {
                        ChatRole::System => {
                            json!({ "role": "system", "content": msg.content })
                        }
                        ChatRole::User => {
                            json!({ "role": "user", "content": msg.content })
                        }
                        ChatRole::Assistant => {
                            if let Some(ref tcs) = msg.tool_calls {
                                // Assistant message that made tool calls — content may be null
                                let tc_array: Vec<Value> = tcs
                                    .iter()
                                    .map(|tc| {
                                        json!({
                                            "id": tc.id,
                                            "type": "function",
                                            "function": {
                                                "name": tc.name,
                                                "arguments": tc.arguments,
                                            }
                                        })
                                    })
                                    .collect();
                                json!({
                                    "role": "assistant",
                                    "content": Value::Null,
                                    "tool_calls": tc_array,
                                })
                            } else {
                                json!({ "role": "assistant", "content": msg.content })
                            }
                        }
                        ChatRole::Tool { tool_call_id } => {
                            json!({
                                "role": "tool",
                                "tool_call_id": tool_call_id,
                                "content": msg.content,
                            })
                        }
                    };
                    msgs.push(json_msg);
                }

                // ── Tool definition ───────────────────────────────────────
                let tools = json!([{
                    "type": "function",
                    "function": {
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
                    }
                }]);

                // ── Build request body ────────────────────────────────────
                let mut body = json!({
                    "model": config.model,
                    "messages": msgs,
                    "stream": true,
                    "tools": tools,
                    "tool_choice": "auto",
                });
                if let Some(t) = config.temperature {
                    body["temperature"] = json!(t);
                }
                if let Some(m) = config.max_tokens {
                    body["max_completion_tokens"] = json!(m);
                }
                if let Some(ref rf) = config.reasoning_format {
                    body["reasoning_format"] = json!(rf);
                }
                if let Some(ref re) = config.reasoning_effort {
                    body["reasoning_effort"] = json!(re);
                }

                let client = reqwest::Client::new();
                let resp = client
                    .post(format!("{}/chat/completions", base_url))
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
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

                // ── Parse SSE stream ──────────────────────────────────────
                // Tool call arguments arrive in fragments across many chunks.
                // We accumulate them by tool-call index, then emit a single
                // ToolCall event when finish_reason == "tool_calls".
                let mut tool_call_accum: std::collections::HashMap<usize, (String, String, String)> =
                    std::collections::HashMap::new(); // index → (id, name, accumulated_arguments)

                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();

                while let Some(chunk) = byte_stream.next().await {
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
                                    if data == "[DONE]" {
                                        let _ = tx.send(AgentEvent::Done);
                                        return;
                                    }
                                    if let Ok(val) = serde_json::from_str::<Value>(data) {
                                        let choice = &val["choices"][0];
                                        let delta = &choice["delta"];
                                        let finish_reason = choice["finish_reason"].as_str();

                                        // Reasoning tokens
                                        if let Some(reasoning) = delta["reasoning"].as_str() {
                                            if !reasoning.is_empty() {
                                                let _ = tx.send(AgentEvent::ReasoningToken(
                                                    reasoning.to_string(),
                                                ));
                                            }
                                        }

                                        // Content tokens
                                        if let Some(content) = delta["content"].as_str() {
                                            if !content.is_empty() {
                                                let _ =
                                                    tx.send(AgentEvent::Token(content.to_string()));
                                            }
                                        }

                                        // Tool call argument fragments
                                        if let Some(tc_array) = delta["tool_calls"].as_array() {
                                            for tc in tc_array {
                                                let index = tc["index"].as_u64().unwrap_or(0)
                                                    as usize;
                                                let entry = tool_call_accum
                                                    .entry(index)
                                                    .or_insert_with(|| {
                                                        (String::new(), String::new(), String::new())
                                                    });
                                                if let Some(id) = tc["id"].as_str() {
                                                    entry.0 = id.to_string();
                                                }
                                                if let Some(name) =
                                                    tc["function"]["name"].as_str()
                                                {
                                                    entry.1 = name.to_string();
                                                }
                                                if let Some(args) =
                                                    tc["function"]["arguments"].as_str()
                                                {
                                                    entry.2.push_str(args);
                                                }
                                            }
                                        }

                                        // Model finished — decide what to emit
                                        match finish_reason {
                                            Some("tool_calls") => {
                                                // Emit one ToolCall event per call (sorted by index)
                                                let mut calls: Vec<_> =
                                                    tool_call_accum.drain().collect();
                                                calls.sort_by_key(|(idx, _)| *idx);
                                                for (_, (id, name, arguments)) in calls {
                                                    let _ = tx.send(AgentEvent::ToolCall {
                                                        id,
                                                        name,
                                                        arguments,
                                                    });
                                                }
                                                let _ = tx.send(AgentEvent::Done);
                                                return;
                                            }
                                            Some("stop") => {
                                                let _ = tx.send(AgentEvent::Done);
                                                return;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Stream ended without an explicit finish_reason
                let _ = tx.send(AgentEvent::Done);
            });
        });

        rx
    }
}
