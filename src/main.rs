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
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use simplelog::{Config, LevelFilter, WriteLogger};

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
}

impl App {
    fn new(provider: Option<Arc<dyn Provider>>) -> Self {
        let welcome = if provider.is_some() {
            "Hello! I'm a helpful assistant.\n(press Ctrl+R to toggle reasoning, Ctrl+D to hide/show tool messages)".to_string()
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
                Ok(AgentEvent::ToolCallResult {
                    info,
                    output,
                }) => {
                    // Merge result into the existing ToolCall message (same invocation).
                    // Walk back to find the matching Tool message for this call id.
                    let merged = self.messages.iter_mut().rev().find(|m| {
                        matches!(m.sender, Sender::Tool)
                            && m.tool_call.as_ref().map(|tc| tc.id == info.id).unwrap_or(false)
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
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                return true; // Signal to quit
            }
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                // Toggle visibility of tool call arguments when Ctrl+T is pressed
                self.show_tool_call_details = !self.show_tool_call_details;
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                // Toggle reasoning/thinking display when Ctrl+R is pressed
                self.show_reasoning = !self.show_reasoning;
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                // Toggle showing of tool messages
                self.show_tools = !self.show_tools;
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

    // Initialise file logger — writes to coder_agent.log next to the binary.
    // All log levels (DEBUG and above) are captured.
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
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
        // All messages shown; Tool details toggle with Ctrl+D.
        .filter(|_msg| true)
        .flat_map(|msg| {
            // Determine display content based on show_tools flag
            let display_content = if matches!(msg.sender, Sender::Tool) && !app.show_tools {
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

            let (label_text, label_style) = match msg.sender {
                Sender::User => (
                    "You",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Sender::Agent => (
                    "Agent",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Sender::Tool => (
                    "Tool",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            };

            let reasoning_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC);

            let mut lines: Vec<Line> = Vec::new();

            // ── Centered label line ───────────────────────────────────────
            let label_len = label_text.chars().count();
            let padding = inner_width.saturating_sub(label_len) / 2;
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(padding)),
                Span::styled(label_text, label_style),
            ]));

            // ── Reasoning block ──────────────────────────────────────────
            if app.show_reasoning && !msg.reasoning.is_empty() {
                let r_indent = "  ";
                let r_available = inner_width.saturating_sub(r_indent.len());
                let mut r_remaining = msg.reasoning.as_str();
                let mut first_r = true;
                while !r_remaining.is_empty() {
                    let take = r_remaining
                        .char_indices()
                        .take_while(|(i, _)| *i < r_available.max(1))
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(r_remaining.len().min(r_available.max(1)));
                    let (chunk, rest) = r_remaining.split_at(take.min(r_remaining.len()));
                    let prefix = if first_r {
                        "  thinking: "
                    } else {
                        "             "
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", prefix, chunk),
                        reasoning_style,
                    )));
                    first_r = false;
                    r_remaining = rest;
                }
            }

            // ── Content block ─────────────────────────────────────────────
            if inner_width == 0 {
                lines.push(Line::from(Span::raw(display_content.clone())));
                lines.push(Line::from(Span::raw("")));
                return lines;
            }

            // Split on newlines then wrap each paragraph to inner_width.
            let paragraphs: Vec<&str> = display_content.split('\n').collect();
            for paragraph in &paragraphs {
                if paragraph.is_empty() {
                    lines.push(Line::from(Span::raw("")));
                    continue;
                }
                let mut chars_remaining = *paragraph;
                while !chars_remaining.is_empty() {
                    let take = chars_remaining
                        .char_indices()
                        .take_while(|(i, _)| *i < inner_width)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(chars_remaining.len().min(inner_width));
                    let (chunk, rest) = chars_remaining.split_at(take.min(chars_remaining.len()));
                    lines.push(Line::from(Span::raw(chunk.to_string())));
                    chars_remaining = rest;
                }
            }

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
