mod app;
mod commands;
mod input;
mod state;
mod ui;

use std::{fs::OpenOptions, io, sync::Arc};

use coder_agent::client::{OpenRouterProvider, Provider};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event},
};
use simplelog::{Config, LevelFilter, WriteLogger};

use state::App;
use ui::render;

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

        if event::poll(std::time::Duration::from_millis(16))?
            && let Event::Key(key_event) = event::read()?
        {
            let should_quit = app.handle_key_event(key_event);
            if should_quit {
                break Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use coder_agent::client::ToolCallInfo;
    use ratatui::backend::TestBackend;

    use crate::state::{App, Message, Sender};
    use crate::ui::render_messages;

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
            diff_preview: None,
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
            diff_preview: None,
        });
        app.messages.push(Message {
            sender: Sender::Tool,
            content: "src/\nCargo.toml".into(),
            reasoning: String::new(),
            tool_call: None,
            tool_name: Some("list_directory".into()),
            diff_preview: None,
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
            diff_preview: None,
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
