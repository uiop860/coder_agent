use std::{
    fs::OpenOptions,
    io,
    sync::{Arc, mpsc::Receiver},
};

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, ChatMessage, OpenRouterProvider, Provider, RequestConfig};
use coder_agent::tools;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout, Margin, Position, Rect, Spacing},
    style::{Color, Modifier, Style},
    symbols::merge::MergeStrategy,
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};
use simplelog::{Config, LevelFilter, WriteLogger};
use unicode_width::UnicodeWidthStr;

// ── Input cursor helpers ────────────────────────────────────────────────────

/// Move one Unicode scalar value to the left, returning the new byte index.
fn cursor_prev_char(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Move one Unicode scalar value to the right, returning the new byte index.
fn cursor_next_char(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos + 1;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Jump one word to the left (bash/readline style): skip whitespace then
/// non-whitespace going backwards.
fn cursor_prev_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = pos;
    // skip trailing spaces
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // skip word chars
    while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

/// Jump one word to the right (bash/readline style): skip non-whitespace then
/// whitespace going forward.
fn cursor_next_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut i = pos;
    // skip word chars
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // skip trailing spaces
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: [SlashCommand; 4] = [
    SlashCommand {
        name: "/reasoning",
        description: "Toggle reasoning / thinking display",
    },
    SlashCommand {
        name: "/tools",
        description: "Toggle tool result messages",
    },
    SlashCommand {
        name: "/model",
        description: "Choose the OpenRouter model",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear the conversation context",
    },
];

struct ModelOption {
    label: &'static str,
    id: &'static str,
    /// Maximum context window in tokens for this model.
    context_window: u64,
}

const MODELS: [ModelOption; 4] = [
    ModelOption {
        label: "Nemotron 3 Nano 30B (free)",
        id: "nvidia/nemotron-3-nano-30b-a3b:free",
        context_window: 256_000,
    },
    ModelOption {
        label: "Trinity Large (free)",
        id: "arcee-ai/trinity-large-preview:free",
        context_window: 131_000,
    },
    ModelOption {
        label: "Step 3.5 Flash (free)",
        id: "stepfun/step-3.5-flash:free",
        context_window: 256_000,
    },
    ModelOption {
        label: "GLM-4.5 Air (free)",
        id: "z-ai/glm-4.5-air:free",
        context_window: 131_072,
    },
];

fn filtered_commands(input: &str) -> Vec<usize> {
    SLASH_COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, c)| c.name.starts_with(input))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone)]
enum Sender {
    User,
    Agent,
    /// Tool call / result messages — toggleable with Ctrl+D.
    Tool,
}

/// A rendered message in the TUI.  Most messages simply carry text and
/// optionally an in-progress reasoning buffer, but we also support a
/// special "tool call" message which stores the parsed information so it can
/// be shown or hidden on demand.
#[derive(Debug, Clone)]
struct Message {
    sender: Sender,
    content: String,
    reasoning: String,
    /// When this message represents a tool call, the parsed details are kept
    /// here so we can re-render it with or without the arguments as the user
    /// toggles `show_tool_call_details`.
    tool_call: Option<coder_agent::client::ToolCallInfo>,
    /// Preserved tool name for tool result messages (when tool_call is None but
    /// sender is Tool). Used to show minimal tool name when show_tools is false.
    tool_name: Option<String>,
}

struct App {
    // Rendered message list
    messages: Vec<Message>,
    // Full conversation history sent to the API
    history: Vec<ChatMessage>,
    input_buffer: String,
    /// Byte index of the edit cursor within `input_buffer`.
    cursor_pos: usize,
    scroll_offset: usize,
    max_scroll: usize,
    scroll_up_held: bool,
    scroll_down_held: bool,
    // Live stream from the provider
    rx: Option<Receiver<AgentEvent>>,
    streaming: bool,
    provider: Option<Arc<dyn Provider>>,
    config: RequestConfig,
    /// When true we render the arguments payload attached to any tool-call
    /// messages.  Toggled by the user pressing Ctrl+T.
    show_tool_call_details: bool,
    /// When false, any reasoning tokens accumulated on messages are hidden
    /// from the view.  Toggled by Ctrl+R.
    show_reasoning: bool,
    /// When false we hide all `Tool`-sender messages.  Toggled by Ctrl+D.
    show_tools: bool,
    /// True when input starts with `/` and the slash-command popover is open.
    slash_mode: bool,
    /// Index of the highlighted row in the filtered slash-command list.
    slash_selected: usize,
    /// True when the model picker sub-menu is open (entered via /model).
    slash_model_mode: bool,
    /// Accumulated input tokens across all completed responses.
    total_input_tokens: u64,
    /// Accumulated output tokens across all completed responses.
    total_output_tokens: u64,
    /// Input token count from the most recent completed response (for context % display).
    last_input_tokens: u64,
    /// Animated display value for input tokens (scrolls toward total_input_tokens each frame).
    displayed_input_tokens: u64,
    /// Animated display value for output tokens (scrolls toward total_output_tokens each frame).
    displayed_output_tokens: u64,
    /// Frame counter used to drive the input-border pulse animation while streaming.
    pulse_tick: u64,
}

impl App {
    fn new(provider: Option<Arc<dyn Provider>>) -> Self {
        let welcome = if provider.is_some() {
            "Hello! I'm a helpful assistant.".to_string()
        } else {
            "⚠ No OPENROUTER_API_KEY found. Set it and restart.".to_string()
        };
        let mut config = RequestConfig::default();
        config.tools = tools::default_tools();
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
        }
    }

    /// Start a streaming request with the current history
    fn send_message(&mut self, text: String) {
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
            self.rx = Some(agent_stream(
                provider,
                self.history.clone(),
                self.config.clone(),
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
    fn poll_stream(&mut self) {
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
                    if let Some(last) = self.messages.last_mut() {
                        if matches!(last.sender, Sender::Agent) && last.content.is_empty() {
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
                    break;
                }
                Err(_) => break, // no more events right now
            }
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> bool {
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
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                return true; // Signal to quit
            }
            (KeyCode::Enter, _) => {
                if !self.input_buffer.is_empty() && !self.streaming {
                    let user_input = self.input_buffer.drain(..).collect();
                    self.cursor_pos = 0;
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

    fn execute_slash_command(&mut self, cmd: &str) {
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

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0; // 0 = follow bottom
    }

    /// Advance animated display values one step toward their targets.
    /// Uses geometric easing: step = max(1, diff / 4) so it closes ~75% of
    /// the remaining gap each frame (~250 ms to settle at 60 fps).
    /// Also advances the pulse animation counter.
    fn tick_token_animation(&mut self) {
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

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    // Initialise file logger — writes to coder_agent.log next to the binary.
    // All log levels (DEBUG and above) are captured.
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("coder_agent.log")?;
    WriteLogger::init(LevelFilter::Debug, Config::default(), log_file)
        .expect("failed to initialise logger");
    log::info!("coder_agent starting");

    let provider = OpenRouterProvider::from_env().map(|p| Arc::new(p) as Arc<dyn Provider>);

    ratatui::run(|terminal| app(terminal, provider))?;
    Ok(())
}

fn app(terminal: &mut DefaultTerminal, provider: Option<Arc<dyn Provider>>) -> io::Result<()> {
    let mut app = App::new(provider);
    // hint about toggle (note: system messages intentionally hidden here too)
    // Help message is skipped to reduce clutter with system messages hidden

    loop {
        app.poll_stream();
        app.tick_token_animation();

        if app.scroll_up_held {
            app.scroll_offset = app.scroll_offset.saturating_add(1).min(app.max_scroll);
        }
        if app.scroll_down_held {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
        }

        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(std::time::Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key_event) => {
                    let should_quit = app.handle_key_event(key_event);
                    if should_quit {
                        break Ok(());
                    }
                }
                _ => {}
            }
        }
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .spacing(Spacing::Overlap(1))
        .split(frame.area());

    // Render status bar at the top
    render_status_bar(frame, chunks[0], app);

    // Render messages area
    render_messages(frame, chunks[1], app);

    // Render slash-command popover (above input, when active)
    if app.slash_mode || app.slash_model_mode {
        render_slash_popover(frame, chunks[2], app);
    }

    // Render input area
    render_input(frame, chunks[2], app);
}

fn render_plain_text(content: &str, text_width: usize, dot_style: Style) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let prefix_dot = "● ";
    let prefix_cont = "  ";
    let paragraphs: Vec<&str> = content.split('\n').collect();
    let mut is_first_line = true;
    for paragraph in &paragraphs {
        if paragraph.is_empty() {
            lines.push(Line::from(Span::raw("")));
            continue;
        }
        let mut chars_remaining = *paragraph;
        while !chars_remaining.is_empty() {
            let take = chars_remaining
                .char_indices()
                .take_while(|(i, _)| *i < text_width)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(chars_remaining.len().min(text_width));
            let (chunk, rest) = chars_remaining.split_at(take.min(chars_remaining.len()));
            if is_first_line {
                lines.push(Line::from(vec![
                    Span::styled(prefix_dot.to_string(), dot_style),
                    Span::raw(chunk.to_string()),
                ]));
                is_first_line = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw(prefix_cont.to_string()),
                    Span::raw(chunk.to_string()),
                ]));
            }
            chars_remaining = rest;
        }
    }
    lines
}
// test
fn render_messages(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(2) as usize; // subtract borders
    let visible_height = area.height.saturating_sub(2) as usize; // subtract borders

    let messages_text: Vec<Line> = app
        .messages
        .iter()
        // All messages shown; Tool details toggle with Ctrl+D.
        .filter(|_msg| true)
        .flat_map(|msg| {
            // Determine display content based on show_tools flag
            let display_content: String = if matches!(msg.sender, Sender::Tool) && !app.show_tools {
                // When tools are hidden, show only the tool name in minimal form
                if let Some(ref tool_name) = msg.tool_name {
                    format!("Tool: {}", tool_name)
                } else if let Some(ref tc) = msg.tool_call {
                    format!("Tool: {}", tc.name)
                } else {
                    "Tool".to_string()
                }
            } else if let Some(tc) = &msg.tool_call {
                let mut s = format!("tool: {} (id={})", tc.name, tc.id);
                if app.show_tool_call_details {
                    s.push_str(&format!("\n  args: {}", tc.arguments));
                }
                s
            } else {
                msg.content.clone()
            };

            let dot_style = match msg.sender {
                Sender::User => Style::default().fg(Color::Blue),
                Sender::Agent => Style::default().fg(Color::Green),
                Sender::Tool => Style::default().fg(Color::Yellow),
            };

            let reasoning_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC);

            let mut lines: Vec<Line> = Vec::new();

            // ── Reasoning (last two word-wrapped lines) ──────────────────────
            if !msg.reasoning.is_empty() {
                let prefix = "  ⟳ ";
                let avail = inner_width.saturating_sub(prefix.len()).max(1);
                // Word-wrap the full reasoning text and keep only the last two lines.
                let wrapped = textwrap::wrap(msg.reasoning.trim_end(), avail);
                let start = wrapped.len().saturating_sub(2);
                for chunk in &wrapped[start..] {
                    lines.push(Line::from(vec![
                        Span::styled(prefix, reasoning_style),
                        Span::styled(chunk.to_string(), reasoning_style),
                    ]));
                }
            }

            // ── Content block ─────────────────────────────────────────────
            if inner_width == 0 {
                lines.push(Line::from(Span::raw(display_content.clone())));
                lines.push(Line::from(Span::raw("")));
                return lines;
            }

            let text_width = inner_width.saturating_sub(2).max(1);

            let display_text = display_content.as_str();
            let content_lines: Vec<Line<'static>>;
            if matches!(msg.sender, Sender::Agent) {
                content_lines = coder_agent::markdown::render_markdown(display_text, text_width);
            } else {
                content_lines = render_plain_text(display_text, text_width, dot_style);
            }
            lines.extend(content_lines);

            // Blank separator between messages
            lines.push(Line::from(Span::raw("")));
            lines
        })
        .collect();

    let total_lines = messages_text.len();
    let auto_scroll = total_lines.saturating_sub(visible_height);
    app.max_scroll = auto_scroll;
    let scroll = auto_scroll.saturating_sub(app.scroll_offset) as u16;

    let messages_widget = Paragraph::new(messages_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Messages (↑/↓ to scroll)")
                .merge_borders(MergeStrategy::Exact),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(messages_widget, area);

    // Scrollbar — only shown when content exceeds the visible area.
    // Ratatui's thumb formula: thumb_end reaches the bottom only when
    // position == content_length - 1, so we remap scroll (0..auto_scroll)
    // to position (0..total_lines-1).
    if auto_scroll > 0 {
        let scrollbar_pos = (scroll as usize) * (total_lines - 1) / auto_scroll;
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .viewport_content_length(visible_height)
            .position(scrollbar_pos);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

fn render_slash_popover(frame: &mut Frame, input_area: Rect, app: &App) {
    if app.slash_model_mode {
        render_model_picker(frame, input_area, app);
        return;
    }

    let filtered = filtered_commands(&app.input_buffer);
    if filtered.is_empty() {
        return;
    }

    let height = filtered.len() as u16 + 2; // +2 for borders
    let popup_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width,
        height,
    };

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(row_idx, &cmd_idx)| {
            let cmd = &SLASH_COMMANDS[cmd_idx];
            let state_label = match cmd.name {
                "/reasoning" => {
                    if app.show_reasoning {
                        "[on]"
                    } else {
                        "[off]"
                    }
                }
                "/tools" => {
                    if app.show_tools {
                        "[on]"
                    } else {
                        "[off]"
                    }
                }
                "/model" => &app.config.model,
                _ => "",
            };
            let text = format!("  {}  –  {}  {}", cmd.name, cmd.description, state_label);
            if row_idx == app.slash_selected {
                ListItem::new(text).style(Style::default().bg(Color::Blue).fg(Color::White))
            } else {
                ListItem::new(text).style(Style::default().fg(Color::Gray))
            }
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Settings"));

    frame.render_widget(list, popup_area);
}

fn render_model_picker(frame: &mut Frame, input_area: Rect, app: &App) {
    let height = MODELS.len() as u16 + 2;
    let popup_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width,
        height,
    };

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = MODELS
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let active = m.id == app.config.model;
            let marker = if active { " ✓" } else { "  " };
            let text = format!("{}  {}  ({})", marker, m.label, m.id);
            if i == app.slash_selected {
                ListItem::new(text).style(Style::default().bg(Color::Blue).fg(Color::White))
            } else if active {
                ListItem::new(text).style(Style::default().fg(Color::Green))
            } else {
                ListItem::new(text).style(Style::default().fg(Color::Gray))
            }
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Choose model  (Enter to select · Esc to cancel)"),
    );

    frame.render_widget(list, popup_area);
}

fn render_input(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let input_text = format!("> {}", app.input_buffer);
    let title = if app.streaming {
        "Input – waiting for response…"
    } else {
        "Input"
    };

    if app.streaming {
        // Render the block normally first (lays out title text and clears interior).
        let input_widget = Paragraph::new(input_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .merge_borders(MergeStrategy::Exact),
            )
            .style(Style::default().fg(Color::White));
        frame.render_widget(input_widget, area);

        // Overpaint each border cell with a per-cell wave color so the highlight
        // travels clockwise around the border.  We do this in its own block so
        // the &mut Buffer borrow is dropped before the set_cursor_position call.
        if area.width >= 2 && area.height >= 2 {
            let w = area.width as usize;
            let h = area.height as usize;
            let total = 2 * (w + h - 2); // total border cells
            // One full revolution every 120 frames (~2 s at 60 fps).
            let time_frac = (app.pulse_tick % 120) as f64 / 120.0;

            // Map a perimeter index to a cool wave color (cyan → indigo → cyan).
            let color_at = |i: usize| -> Color {
                let phase = (i as f64 / total as f64 + time_frac) % 1.0;
                let t = (std::f64::consts::TAU * phase).cos().mul_add(-0.5, 0.5);
                let r = (t * 110.0) as u8;
                let g = (220.0_f64 - t * 180.0) as u8;
                Color::Rgb(r, g, 255)
            };

            let buf = frame.buffer_mut();
            let mut idx = 0usize;

            // Top row: left → right
            for x in 0..w as u16 {
                buf[(area.x + x, area.y)].set_fg(color_at(idx));
                idx += 1;
            }
            // Right column: top+1 → bottom-1
            for y in 1..(h - 1) as u16 {
                buf[(area.x + area.width - 1, area.y + y)].set_fg(color_at(idx));
                idx += 1;
            }
            // Bottom row: right → left
            for x in (0..w as u16).rev() {
                buf[(area.x + x, area.y + area.height - 1)].set_fg(color_at(idx));
                idx += 1;
            }
            // Left column: bottom-1 → top+1
            for y in (1..(h - 1) as u16).rev() {
                buf[(area.x, area.y + y)].set_fg(color_at(idx));
                idx += 1;
            }
        }
    } else {
        let input_widget = Paragraph::new(input_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .merge_borders(MergeStrategy::Exact),
            )
            .style(Style::default().fg(Color::Rgb(130, 60, 255)));
        frame.render_widget(input_widget, area);
    }

    // Set cursor position in the input area.
    // Skip while animating or streaming: ratatui moves the physical terminal
    // cursor through every changed cell during diff painting.  When the status
    // bar numbers are changing each frame the cursor visibly drags across them
    // before being repositioned here.  Not calling set_cursor_position hides
    // the cursor for those frames, which looks much cleaner.
    let animating = app.displayed_input_tokens != app.total_input_tokens
        || app.displayed_output_tokens != app.total_output_tokens;
    if !app.streaming && !animating && area.height > 0 {
        let before_cursor = &app.input_buffer[..app.cursor_pos];
        let display_width = UnicodeWidthStr::width(before_cursor) as u16;
        let cursor_x = area.x + 1 + 2 + display_width;
        let cursor_y = area.y + 1;
        if cursor_x < area.right() && cursor_y < area.bottom() {
            frame.set_cursor_position(Position::new(cursor_x.min(area.right() - 1), cursor_y));
        }
    }
}

/// Return the context-window size for a known model id, or a generic fallback.
fn model_context_window(model_id: &str) -> u64 {
    MODELS
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| m.context_window)
        .unwrap_or(128_000) // safe fallback for unknown models
}

fn render_status_bar(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let total = app.displayed_input_tokens + app.displayed_output_tokens;

    // Context percentage — based on the last request's input token count.
    let ctx_window = model_context_window(&app.config.model);
    let (ctx_str, ctx_color) = if app.last_input_tokens == 0 {
        ("–".to_string(), Color::DarkGray)
    } else {
        let pct = app.last_input_tokens as f64 / ctx_window as f64 * 100.0;
        let color = if pct >= 90.0 {
            Color::Red
        } else if pct >= 70.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        (format!("{:.1}%", pct), color)
    };

    let line = Line::from(vec![
        Span::styled("  In: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", app.displayed_input_tokens),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("  Out: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", app.displayed_output_tokens),
            Style::default().fg(Color::Green),
        ),
        Span::styled("  Total: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", total), Style::default().fg(Color::Yellow)),
        Span::styled("   │  Ctx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(ctx_str, Style::default().fg(ctx_color)),
        Span::styled(
            format!(" /{}", ctx_window),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("   │  Model: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.config.model.clone(),
            Style::default().fg(Color::Magenta),
        ),
    ]);

    let widget = Paragraph::new(vec![line]).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Session")
            .merge_borders(MergeStrategy::Exact),
    );
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use coder_agent::client::ToolCallInfo;
    use ratatui::backend::TestBackend;

    fn make_terminal() -> ratatui::Terminal<TestBackend> {
        ratatui::Terminal::new(TestBackend::new(80, 20)).unwrap()
    }

    #[test]
    fn tool_toggle_does_not_panic() {
        let mut terminal = make_terminal();
        let mut app = App::new(None);
        app.messages.push(Message {
            sender: Sender::Tool,
            content: "tool output".into(),
            reasoning: String::new(),
            tool_call: None,
            tool_name: Some("list_directory".into()),
        });
        app.show_tools = false; // toggle off
        terminal
            .draw(|f| render_messages(f, f.area(), &mut app))
            .unwrap();
    }

    /// Tool messages are visible when show_tools is true.
    #[test]
    fn tool_messages_visible_when_enabled() {
        let mut terminal = make_terminal();
        let mut app = App::new(None);
        // show_tools defaults to false; enable it for this test
        app.show_tools = true;
        assert!(app.show_tools);

        let tc = ToolCallInfo {
            id: "call_1".into(),
            name: "list_directory".into(),
            arguments: r#"{"path":"."}"#.into(),
        };
        app.messages.push(Message {
            sender: Sender::Tool,
            content: String::new(),
            reasoning: String::new(),
            tool_call: Some(tc.clone()),
            tool_name: Some("list_directory".into()),
        });
        app.messages.push(Message {
            sender: Sender::Tool,
            content: "src/\nCargo.toml".into(),
            reasoning: String::new(),
            tool_call: None,
            tool_name: Some("list_directory".into()),
        });

        // Should render without panic and produce lines containing tool info
        let mut rendered_lines = 0usize;
        terminal
            .draw(|f| {
                render_messages(f, f.area(), &mut app);
                // Count lines generated — at least the two Tool messages should appear
                rendered_lines = app
                    .messages
                    .iter()
                    .filter(|m| matches!(m.sender, Sender::Tool))
                    .count();
            })
            .unwrap();

        assert_eq!(rendered_lines, 2, "both Tool messages must be counted");
    }

    /// Tool messages are always shown but with different rendering based on show_tools.
    #[test]
    fn tool_toggleable() {
        let mut terminal = make_terminal();
        let mut app = App::new(None);

        let tc = ToolCallInfo {
            id: "call_1".into(),
            name: "list_directory".into(),
            arguments: r#"{"path":"."}"#.into(),
        };

        app.messages.push(Message {
            sender: Sender::Tool,
            content: "detailed output".into(),
            reasoning: String::new(),
            tool_call: Some(tc),
            tool_name: Some("list_directory".into()),
        });

        // With show_tools = true, tool should show full details
        app.show_tools = true;
        terminal
            .draw(|f| render_messages(f, f.area(), &mut app))
            .unwrap();

        // With show_tools = false, tool should show minimal (tool name only)
        app.show_tools = false;
        terminal
            .draw(|f| render_messages(f, f.area(), &mut app))
            .unwrap();

        // No panic = success
        assert!(!app.show_tools);
    }
}
