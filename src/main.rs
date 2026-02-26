use std::{
    io,
    sync::{Arc, mpsc::Receiver},
};

use coder_agent::agent::agent_stream;
use coder_agent::client::{AgentEvent, CerebrasProvider, ChatMessage, Provider, RequestConfig};
use coder_agent::executor::JsExecutorHandle;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

#[derive(Debug, Clone)]
enum Sender {
    User,
    Agent,
    System,
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
}

struct App {
    // Rendered message list
    messages: Vec<Message>,
    // Full conversation history sent to the API
    history: Vec<ChatMessage>,
    input_buffer: String,
    scroll_offset: usize,
    max_scroll: usize,
    scroll_up_held: bool,
    scroll_down_held: bool,
    // Live stream from the provider
    rx: Option<Receiver<AgentEvent>>,
    streaming: bool,
    provider: Option<Arc<dyn Provider>>,
    executor: Option<JsExecutorHandle>,
    config: RequestConfig,
    /// When true we render the arguments payload attached to any tool-call
    /// messages.  Toggled by the user pressing 't'.
    show_tool_call_details: bool,
    /// When false, any reasoning tokens accumulated on messages are hidden
    /// from the view.  Toggled by Ctrl+R.
    show_reasoning: bool,
    /// When false we hide all `System`-sender messages (useful for decluttering).
    show_system: bool,
}

impl App {
    fn new(provider: Option<Arc<dyn Provider>>, executor: Option<JsExecutorHandle>) -> Self {
        let welcome = if provider.is_some() {
            "Hello! I'm your code agent. I can write and execute TypeScript for you.\n(press Ctrl+T to toggle tool-call details, Ctrl+R to toggle reasoning, Ctrl+D to hide/show system messages)".to_string()
        } else {
            "⚠ No CEREBRAS_API_KEY found. Set it and restart.".to_string()
        };
        Self {
            messages: vec![Message {
                sender: Sender::Agent,
                content: welcome,
                reasoning: String::new(),
                tool_call: None,
            }],
            history: Vec::new(),
            input_buffer: String::new(),
            scroll_offset: 0,
            max_scroll: 0,
            scroll_up_held: false,
            scroll_down_held: false,
            rx: None,
            streaming: false,
            provider,
            executor,
            config: RequestConfig::default(),
            show_tool_call_details: false,
            show_reasoning: false,
            show_system: false,
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
        });
        // Add to API history
        self.history.push(ChatMessage::user(text));
        // Placeholder for the incoming response
        self.messages.push(Message {
            sender: Sender::Agent,
            content: String::new(),
            reasoning: String::new(),
            tool_call: None,
        });

        if let (Some(provider), Some(executor)) = (self.provider.clone(), self.executor.clone()) {
            self.rx = Some(agent_stream(
                provider,
                executor,
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
                Ok(AgentEvent::Done) => {
                    // Persist completed response into history, then clear reasoning
                    if let Some(last) = self.messages.last_mut() {
                        self.history
                            .push(ChatMessage::assistant(last.content.clone()));
                        last.reasoning.clear();
                    }
                    self.rx = None;
                    self.streaming = false;
                    break;
                }
                Ok(AgentEvent::Error(e)) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content = format!("[error] {}", e);
                    }
                    self.rx = None;
                    self.streaming = false;
                    break;
                }
                Ok(AgentEvent::ScriptStarting) => {
                    self.messages.push(Message {
                        sender: Sender::System,
                        content: "⚙ Executing script…".to_string(),
                        reasoning: String::new(),
                        tool_call: None,
                    });
                    self.scroll_to_bottom();
                }
                Ok(AgentEvent::ScriptOutput(output)) => {
                    self.messages.push(Message {
                        sender: Sender::System,
                        content: format!("✓ Script output: {}", output),
                        reasoning: String::new(),
                        tool_call: None,
                    });
                    // Add a new placeholder for the next LLM response
                    self.messages.push(Message {
                        sender: Sender::Agent,
                        content: String::new(),
                        reasoning: String::new(),
                        tool_call: None,
                    });
                    self.scroll_to_bottom();
                }
                Ok(AgentEvent::ScriptError(err)) => {
                    self.messages.push(Message {
                        sender: Sender::System,
                        content: format!("✗ Script error: {}", err),
                        reasoning: String::new(),
                        tool_call: None,
                    });
                    // Add a new placeholder for the next LLM response
                    self.messages.push(Message {
                        sender: Sender::Agent,
                        content: String::new(),
                        reasoning: String::new(),
                        tool_call: None,
                    });
                    self.scroll_to_bottom();
                }
                Ok(AgentEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    // Show a system message for the incoming tool call; the
                    // agent loop will also see it separately.
                    let info = coder_agent::client::ToolCallInfo {
                        id,
                        name,
                        arguments,
                    };
                    self.messages.push(Message {
                        sender: Sender::System,
                        content: String::new(),
                        reasoning: String::new(),
                        tool_call: Some(info),
                    });
                    self.scroll_to_bottom();
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
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                return true; // Signal to quit
            }
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                // Toggle visibility of tool call arguments when Ctrl+T is pressed
                self.show_tool_call_details = !self.show_tool_call_details;
                // re-rendering on next frame will reflect the change
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                // Toggle reasoning/thinking display when Ctrl+R is pressed
                self.show_reasoning = !self.show_reasoning;
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                // Toggle showing of system/debug messages
                self.show_system = !self.show_system;
            }
            (KeyCode::Enter, _) => {
                if !self.input_buffer.is_empty() && !self.streaming {
                    let user_input = self.input_buffer.drain(..).collect();
                    self.send_message(user_input);
                }
            }
            (KeyCode::Up, _) => {
                self.scroll_up_held = true;
                self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
            }
            (KeyCode::Down, _) => {
                self.scroll_down_held = true;
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            (KeyCode::Backspace, _) => {
                self.input_buffer.pop();
            }
            (KeyCode::Char(c), _) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
        false
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0; // 0 = follow bottom
    }
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    // Initialize V8 platform (must happen before any isolate is created)
    let platform = v8::new_unprotected_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let provider = CerebrasProvider::from_env().map(|p| Arc::new(p) as Arc<dyn Provider>);
    let executor = Some(JsExecutorHandle::spawn());

    ratatui::run(|terminal| app(terminal, provider, executor))?;
    Ok(())
}

fn app(
    terminal: &mut DefaultTerminal,
    provider: Option<Arc<dyn Provider>>,
    executor: Option<JsExecutorHandle>,
) -> io::Result<()> {
    let mut app = App::new(provider, executor);
    // hint about toggle
    if app.provider.is_some() {
        app.messages.push(Message {
            sender: Sender::System,
            content:
                "(Press Ctrl+T to show/hide tool-call arguments; Ctrl+R to show/hide reasoning; Ctrl+D to hide/show system messages)"
                    .to_string(),
            reasoning: String::new(),
            tool_call: None,
        });
    }

    loop {
        app.poll_stream();

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
        .constraints([Constraint::Percentage(80), Constraint::Percentage(20)])
        .split(frame.area());

    // Render messages area
    render_messages(frame, chunks[0], app);

    // Render input area
    render_input(frame, chunks[1], app);
}

fn render_messages(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(2) as usize; // subtract borders
    let visible_height = area.height.saturating_sub(2) as usize; // subtract borders

    let messages_text: Vec<Line> = app
        .messages
        .iter()
        .filter(|msg| app.show_system || !matches!(msg.sender, Sender::System))
        .flat_map(|msg| {
            // choose what text we actually display for this message; if it has
            // an attached tool-call payload we build a summary/arguments string
            // dynamically so that toggling `show_tool_call_details` will affect
            // existing messages as well.
            let display_content = if let Some(tc) = &msg.tool_call {
                let mut s = format!("🔧 Tool call: {} (id={})", tc.name, tc.id);
                if app.show_tool_call_details {
                    s.push_str(&format!("\narguments: {}", tc.arguments));
                }
                s
            } else {
                msg.content.clone()
            };

            let (label, prefix_len) = match msg.sender {
                Sender::User => (Span::styled("You: ", Style::default().fg(Color::Blue)), 5),
                Sender::Agent => (
                    Span::styled("Agent: ", Style::default().fg(Color::Green)),
                    7,
                ),
                Sender::System => (
                    Span::styled("System: ", Style::default().fg(Color::Cyan)),
                    8,
                ),
            };

            let reasoning_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC);

            let mut lines: Vec<Line> = Vec::new();

            // ── Reasoning block ──────────────────────────────────────────
            if app.show_reasoning && !msg.reasoning.is_empty() {
                let r_prefix = "  thinking: ";
                let r_prefix_len = r_prefix.chars().count();
                let r_available = inner_width.saturating_sub(r_prefix_len);
                let mut first_r = true;
                let mut r_remaining = msg.reasoning.as_str();
                while !r_remaining.is_empty() {
                    let take = r_remaining
                        .char_indices()
                        .take_while(|(i, _)| *i < r_available.max(1))
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(r_remaining.len().min(r_available.max(1)));
                    let (chunk, rest) = r_remaining.split_at(take.min(r_remaining.len()));
                    if first_r {
                        lines.push(Line::from(vec![
                            Span::styled(r_prefix, reasoning_style),
                            Span::styled(chunk.to_string(), reasoning_style),
                        ]));
                        first_r = false;
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("{}{}", " ".repeat(r_prefix_len), chunk),
                            reasoning_style,
                        )));
                    }
                    r_remaining = rest;
                }
            }

            // ── Content block ──────────────────────────────────────────
            let available = inner_width.saturating_sub(prefix_len);
            if available == 0 || app.messages.is_empty() {
                lines.push(Line::from(vec![label, Span::raw(display_content.clone())]));
                return lines;
            }

            // Split on newlines first, then word-wrap each paragraph so that
            // code blocks and multi-line LLM responses render correctly.
            let paragraphs: Vec<&str> = display_content.split('\n').collect();
            let mut first = true;
            for paragraph in &paragraphs {
                // Empty paragraph = blank line (just indent)
                if paragraph.is_empty() {
                    if first {
                        lines.push(Line::from(vec![label.clone(), Span::raw(String::new())]));
                        first = false;
                    } else {
                        lines.push(Line::from(Span::raw(" ".repeat(prefix_len))));
                    }
                    continue;
                }

                let mut chars_remaining = *paragraph;
                while !chars_remaining.is_empty() {
                    let take = chars_remaining
                        .char_indices()
                        .take_while(|(i, _)| *i < available)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(chars_remaining.len().min(available));
                    let (chunk, rest) = chars_remaining.split_at(take.min(chars_remaining.len()));
                    if first {
                        lines.push(Line::from(vec![
                            label.clone(),
                            Span::raw(chunk.to_string()),
                        ]));
                        first = false;
                    } else {
                        lines.push(Line::from(Span::raw(format!(
                            "{}{}",
                            " ".repeat(prefix_len),
                            chunk
                        ))));
                    }
                    chars_remaining = rest;
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(vec![label, Span::raw(String::new())]));
            }
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
                .title("Messages (↑/↓ to scroll)"),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(messages_widget, area);
}

fn render_input(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let input_text = format!("> {}", app.input_buffer);
    let title = if app.streaming {
        "Input – waiting for response… (Ctrl+C to quit)"
    } else {
        "Input (Enter to send · Ctrl+C to quit)"
    };
    let style = if app.streaming {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Yellow)
    };

    let input_widget = Paragraph::new(input_text)
        .block(Block::default().borders(Borders::ALL).title(title))
        .style(style);

    frame.render_widget(input_widget, area);

    // Set cursor position in the input area
    if area.height > 0 {
        let cursor_x = (app.input_buffer.len() as u16) + 3; // +3 for "> " prefix
        let cursor_y = area.y + 1;
        if cursor_x < area.right() && cursor_y < area.bottom() {
            frame.set_cursor_position(Position::new(cursor_x.min(area.right() - 1), cursor_y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn show_system_toggle_does_not_panic() {
        // Construct a minimal App and flip the flag; call render to ensure
        // filtering logic is exercised.
        let backend = TestBackend::new(10, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut app = App::new(None, None);
        app.messages.push(Message {
            sender: Sender::System,
            content: "debug".into(),
            reasoning: String::new(),
            tool_call: None,
        });
        app.show_system = false;
        terminal
            .draw(|f| {
                render_messages(
                    f,
                    ratatui::layout::Rect {
                        x: 0,
                        y: 0,
                        width: 10,
                        height: 5,
                    },
                    &mut app,
                );
            })
            .unwrap();
    }
}
