use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, ChatMessage, Provider, RequestConfig};
use coder_agent::tools;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::commands::{MODELS, SLASH_COMMANDS, filtered_commands};
use crate::input::{cursor_next_char, cursor_next_word, cursor_prev_char, cursor_prev_word};
use crate::state::{App, Message, Sender};

impl App {
    pub fn new(provider: Option<Arc<dyn Provider>>) -> Self {
        let welcome = if provider.is_some() {
            "Hello! I'm a helpful assistant.".to_string()
        } else {
            "⚠ No OPENROUTER_API_KEY found. Set it and restart.".to_string()
        };
        let config = RequestConfig {
            tools: tools::default_tools(),
            ..RequestConfig::default()
        };
        Self {
            messages: vec![Message {
                sender: Sender::Agent,
                content: welcome,
                reasoning: String::new(),
                tool_call: None,
                tool_name: None,
            }],
            history: Vec::new(),
            input_buffer: String::new(),
            cursor_pos: 0,
            input_scroll: 0,
            scroll_offset: 0,
            max_scroll: 0,
            scroll_up_held: false,
            scroll_down_held: false,
            rx: None,
            streaming: false,
            provider,
            config,
            show_tool_call_details: false,
            show_reasoning: false,
            show_tools: false,
            slash_mode: false,
            slash_selected: 0,
            slash_model_mode: false,
            total_input_tokens: 0,
            total_output_tokens: 0,
            last_input_tokens: 0,
            displayed_input_tokens: 0,
            displayed_output_tokens: 0,
            pulse_tick: 0,
            cancel: None,
            approval_tx: None,
            approval_pending: None,
        }
    }

    /// Start a streaming request with the current history
    pub fn send_message(&mut self, text: String) {
        // Add to render list
        self.messages.push(Message {
            sender: Sender::User,
            content: text.clone(),
            reasoning: String::new(),
            tool_call: None,
            tool_name: None,
        });
        // Add to API history
        self.history.push(ChatMessage::user(text));
        // Placeholder for the incoming response
        self.messages.push(Message {
            sender: Sender::Agent,
            content: String::new(),
            reasoning: String::new(),
            tool_call: None,
            tool_name: None,
        });

        if let Some(provider) = self.provider.clone() {
            let cancel_flag = Arc::new(AtomicBool::new(false));
            self.cancel = Some(cancel_flag.clone());
            let (approval_tx, approval_rx) = std::sync::mpsc::channel::<bool>();
            self.approval_tx = Some(approval_tx);
            self.rx = Some(agent_stream(
                provider,
                self.history.clone(),
                self.config.clone(),
                cancel_flag,
                approval_rx,
            ));
            self.streaming = true;
            self.scroll_to_bottom();
        } else {
            // No provider – show error in placeholder
            if let Some(last) = self.messages.last_mut() {
                last.content = "No provider configured.".to_string();
            }
        }
    }

    /// Drain pending events from the active stream. Call once per frame.
    pub fn poll_stream(&mut self) {
        if self.rx.is_none() {
            return;
        }
        loop {
            match self.rx.as_ref().unwrap().try_recv() {
                Ok(AgentEvent::Token(t)) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&t);
                        self.scroll_to_bottom();
                    }
                }
                Ok(AgentEvent::ReasoningToken(t)) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.reasoning.push_str(&t);
                        self.scroll_to_bottom();
                    }
                }
                Ok(AgentEvent::ToolCall(tc)) => {
                    // Replace the empty Agent placeholder with the tool call message
                    // so there's no blank Agent line above it.
                    let tool_name = tc.name.clone();
                    if let Some(last) = self.messages.last_mut()
                        && matches!(last.sender, Sender::Agent)
                        && last.content.is_empty()
                    {
                        *last = Message {
                            sender: Sender::Tool,
                            content: String::new(),
                            reasoning: String::new(),
                            tool_call: Some(tc),
                            tool_name: Some(tool_name.clone()),
                        };
                        self.scroll_to_bottom();
                        continue;
                    }
                    self.messages.push(Message {
                        sender: Sender::Tool,
                        content: String::new(),
                        reasoning: String::new(),
                        tool_call: Some(tc),
                        tool_name: Some(tool_name),
                    });
                    self.scroll_to_bottom();
                }
                Ok(AgentEvent::ToolCallResult { info, output }) => {
                    // Merge result into the existing ToolCall message (same invocation).
                    // Walk back to find the matching Tool message for this call id.
                    let merged = self.messages.iter_mut().rev().find(|m| {
                        matches!(m.sender, Sender::Tool)
                            && m.tool_call
                                .as_ref()
                                .map(|tc| tc.id == info.id)
                                .unwrap_or(false)
                    });
                    if let Some(msg) = merged {
                        msg.content = output;
                    } else {
                        // Fallback: no matching call message found, create a new one.
                        self.messages.push(Message {
                            sender: Sender::Tool,
                            content: output,
                            reasoning: String::new(),
                            tool_call: None,
                            tool_name: Some(info.name),
                        });
                    }
                    // Placeholder for the next LLM response
                    self.messages.push(Message {
                        sender: Sender::Agent,
                        content: String::new(),
                        reasoning: String::new(),
                        tool_call: None,
                        tool_name: None,
                    });
                    self.scroll_to_bottom();
                }
                Ok(AgentEvent::ToolApprovalRequest { info }) => {
                    self.approval_pending = Some(info);
                    // Agent thread is now blocked; TUI will show modal
                    break;
                }
                Ok(AgentEvent::Done) => {
                    // Persist completed response into history, then clear reasoning
                    if let Some(last) = self.messages.last_mut() {
                        // Only add non-empty content to history
                        if !last.content.is_empty() {
                            self.history
                                .push(ChatMessage::assistant(last.content.clone()));
                        }
                        last.reasoning.clear();
                    }
                    self.rx = None;
                    self.streaming = false;
                    self.cancel = None;
                    self.approval_tx = None;
                    self.approval_pending = None;
                    break;
                }
                Ok(AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                }) => {
                    self.total_input_tokens += input_tokens;
                    self.total_output_tokens += output_tokens;
                    self.last_input_tokens = input_tokens;
                }
                Ok(AgentEvent::Error(e)) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content = format!("[error] {}", e);
                    }
                    self.rx = None;
                    self.streaming = false;
                    self.cancel = None;
                    self.approval_tx = None;
                    self.approval_pending = None;
                    break;
                }
                Err(_) => break, // no more events right now
            }
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        // ── Approval modal — highest priority ────────────────────────────────
        if self.approval_pending.is_some() {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        if let Some(tx) = &self.approval_tx {
                            let _ = tx.send(true);
                        }
                        self.approval_pending = None;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        if let Some(tx) = &self.approval_tx {
                            let _ = tx.send(false);
                        }
                        self.approval_pending = None;
                    }
                    _ => {}
                }
            }
            return false; // consume all events while modal is active
        }

        match key.kind {
            KeyEventKind::Release => {
                match key.code {
                    KeyCode::Up => self.scroll_up_held = false,
                    KeyCode::Down => self.scroll_down_held = false,
                    _ => {}
                }
                return false;
            }
            KeyEventKind::Press => {}
            _ => return false,
        }

        // ── Model-picker sub-menu ────────────────────────────────────────────
        if self.slash_model_mode {
            match key.code {
                KeyCode::Up => {
                    self.slash_selected = self.slash_selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    self.slash_selected = (self.slash_selected + 1).min(MODELS.len() - 1);
                }
                KeyCode::Enter => {
                    self.config.model = MODELS[self.slash_selected].id.to_string();
                    self.slash_model_mode = false;
                    self.slash_mode = false;
                    self.slash_selected = 0;
                    self.input_buffer.clear();
                    self.cursor_pos = 0;
                }
                KeyCode::Esc => {
                    self.slash_model_mode = false;
                    self.slash_mode = false;
                    self.slash_selected = 0;
                    self.input_buffer.clear();
                    self.cursor_pos = 0;
                }
                _ => {}
            }
            return false;
        }

        // ── Slash-mode navigation ────────────────────────────────────────────
        if self.slash_mode {
            let filtered = filtered_commands(&self.input_buffer);
            match key.code {
                KeyCode::Up => {
                    self.slash_selected = self.slash_selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    if !filtered.is_empty() {
                        self.slash_selected = (self.slash_selected + 1).min(filtered.len() - 1);
                    }
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.cursor_pos = 0;
                    self.slash_mode = false;
                    self.slash_selected = 0;
                }
                KeyCode::Enter => {
                    if !filtered.is_empty() {
                        let cmd = SLASH_COMMANDS[filtered[self.slash_selected]].name;
                        if cmd == "/model" {
                            // Open model picker sub-menu; pre-select current model
                            let current = self.config.model.as_str();
                            self.slash_selected =
                                MODELS.iter().position(|m| m.id == current).unwrap_or(0);
                            self.slash_model_mode = true;
                        } else if cmd == "/exit" {
                            return true;
                        } else {
                            self.execute_slash_command(cmd);
                            self.input_buffer.clear();
                            self.cursor_pos = 0;
                            self.slash_mode = false;
                            self.slash_selected = 0;
                        }
                    } else {
                        self.input_buffer.clear();
                        self.cursor_pos = 0;
                        self.slash_mode = false;
                        self.slash_selected = 0;
                    }
                }
                KeyCode::Backspace => {
                    self.input_buffer.pop();
                    self.cursor_pos = self.input_buffer.len();
                    if self.input_buffer.is_empty() {
                        self.slash_mode = false;
                    }
                    self.slash_selected = 0;
                }
                KeyCode::Char(c) => {
                    self.input_buffer.push(c);
                    self.cursor_pos = self.input_buffer.len();
                    self.slash_selected = 0;
                }
                _ => {}
            }
            return false;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                return true; // Always quit
            }
            (KeyCode::Esc, _) => {
                if self.streaming {
                    if let Some(c) = &self.cancel {
                        c.store(true, Ordering::Relaxed);
                    }
                    self.rx = None;
                    self.streaming = false;
                    self.cancel = None;
                    self.approval_tx = None;
                    self.approval_pending = None;
                    if let Some(last) = self.messages.last_mut()
                        && matches!(last.sender, Sender::Agent)
                        && last.content.is_empty()
                    {
                        last.content = "[cancelled]".to_string();
                    }
                } else {
                    return true; // quit
                }
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                self.input_buffer.insert(self.cursor_pos, '\n');
                self.cursor_pos += 1;
            }
            (KeyCode::Enter, _) => {
                if !self.input_buffer.is_empty() && !self.streaming {
                    let user_input = self.input_buffer.drain(..).collect();
                    self.cursor_pos = 0;
                    self.input_scroll = 0;
                    self.send_message(user_input);
                }
            }
            // ── Scroll ──────────────────────────────────────────────────────
            (KeyCode::Up, _) => {
                self.scroll_up_held = true;
                self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
            }
            (KeyCode::Down, _) => {
                self.scroll_down_held = true;
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            // ── Cursor movement ─────────────────────────────────────────────
            (KeyCode::Left, KeyModifiers::CONTROL) => {
                self.cursor_pos = cursor_prev_word(&self.input_buffer, self.cursor_pos);
            }
            (KeyCode::Right, KeyModifiers::CONTROL) => {
                self.cursor_pos = cursor_next_word(&self.input_buffer, self.cursor_pos);
            }
            (KeyCode::Left, _) => {
                self.cursor_pos = cursor_prev_char(&self.input_buffer, self.cursor_pos);
            }
            (KeyCode::Right, _) => {
                self.cursor_pos = cursor_next_char(&self.input_buffer, self.cursor_pos);
            }
            (KeyCode::Home, _) => {
                self.cursor_pos = 0;
            }
            (KeyCode::End, _) => {
                self.cursor_pos = self.input_buffer.len();
            }
            // ── Deletion ────────────────────────────────────────────────────
            (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                // Delete from previous word boundary up to cursor
                let start = cursor_prev_word(&self.input_buffer, self.cursor_pos);
                self.input_buffer.drain(start..self.cursor_pos);
                self.cursor_pos = start;
            }
            (KeyCode::Backspace, _) => {
                if self.cursor_pos > 0 {
                    let prev = cursor_prev_char(&self.input_buffer, self.cursor_pos);
                    self.input_buffer.remove(prev);
                    self.cursor_pos = prev;
                }
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) => {
                // Delete from cursor to next word boundary
                let end = cursor_next_word(&self.input_buffer, self.cursor_pos);
                self.input_buffer.drain(self.cursor_pos..end);
            }
            (KeyCode::Delete, _) => {
                if self.cursor_pos < self.input_buffer.len() {
                    self.input_buffer.remove(self.cursor_pos);
                }
            }
            // ── Typing ──────────────────────────────────────────────────────
            (KeyCode::Char(c), _) => {
                if c == '/' && self.input_buffer.is_empty() {
                    self.slash_mode = true;
                    self.slash_selected = 0;
                }
                self.input_buffer.insert(self.cursor_pos, c);
                self.cursor_pos += c.len_utf8();
            }
            _ => {}
        }
        false
    }

    pub fn execute_slash_command(&mut self, cmd: &str) {
        match cmd {
            "/reasoning" => self.show_reasoning = !self.show_reasoning,
            "/tools" => self.show_tools = !self.show_tools,
            "/clear" => {
                self.messages.clear();
                self.history.clear();
                self.scroll_offset = 0;
                self.total_input_tokens = 0;
                self.total_output_tokens = 0;
                self.last_input_tokens = 0;
                self.displayed_input_tokens = 0;
                self.displayed_output_tokens = 0;
            }
            _ => {}
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0; // 0 = follow bottom
    }

    /// Advance animated display values one step toward their targets.
    /// Uses geometric easing: step = max(1, diff / 4) so it closes ~75% of
    /// the remaining gap each frame (~250 ms to settle at 60 fps).
    /// Also advances the pulse animation counter.
    pub fn tick_token_animation(&mut self) {
        self.pulse_tick = self.pulse_tick.wrapping_add(1);
        let step_in = ((self
            .total_input_tokens
            .saturating_sub(self.displayed_input_tokens))
            / 4)
        .max(1);
        self.displayed_input_tokens =
            (self.displayed_input_tokens + step_in).min(self.total_input_tokens);

        let step_out = ((self
            .total_output_tokens
            .saturating_sub(self.displayed_output_tokens))
            / 4)
        .max(1);
        self.displayed_output_tokens =
            (self.displayed_output_tokens + step_out).min(self.total_output_tokens);
    }
}
