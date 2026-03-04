use ratatui::{
    Frame,
    layout::{Margin, Position, Rect},
    style::{Color, Modifier, Style},
    symbols::merge::MergeStrategy,
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

use crate::commands::{MODELS, SLASH_COMMANDS, filtered_commands};
use crate::input::cursor_row_col;
use crate::state::{App, Sender};

pub fn truncate_at_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .take_while(|(i, _)| *i < max)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(max);
    format!("{}...", &s[..end])
}

fn model_context_window(model_id: &str) -> u64 {
    MODELS
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| m.context_window)
        .unwrap_or(128_000) // safe fallback for unknown models
}

pub fn render(frame: &mut Frame, app: &mut App) {
    use ratatui::layout::{Constraint, Direction, Layout, Spacing};

    let input_lines = app.input_buffer.chars().filter(|&c| c == '\n').count() + 1;
    let input_height = (input_lines as u16 + 2).min(10); // +2 borders, cap 10

    let visible_inner = input_height.saturating_sub(2); // inner height without borders
    let (cur_row, _) = cursor_row_col(&app.input_buffer, app.cursor_pos);
    if cur_row < app.input_scroll {
        app.input_scroll = cur_row;
    }
    if visible_inner > 0 && cur_row >= app.input_scroll + visible_inner {
        app.input_scroll = cur_row - visible_inner + 1;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(input_height),
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

    // Render approval dialog if waiting for approval
    if let Some(ref info) = app.approval_pending {
        render_approval_dialog(frame, frame.area(), info);
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

pub fn render_messages(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
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
            let content_lines: Vec<Line<'static>> = if matches!(msg.sender, Sender::Agent) {
                coder_agent::markdown::render_markdown(display_text, text_width)
            } else {
                render_plain_text(display_text, text_width, dot_style)
            };
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
    let input_text = app.input_buffer.clone();
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
            .scroll((app.input_scroll, 0))
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
            .scroll((app.input_scroll, 0))
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
        let (cur_row, cur_col) = cursor_row_col(&app.input_buffer, app.cursor_pos);
        let visible_row = cur_row.saturating_sub(app.input_scroll);
        let cursor_x = area.x + 1 + cur_col; // +1 for border
        let cursor_y = area.y + 1 + visible_row; // +1 for border
        if cursor_x < area.right() && cursor_y < area.bottom() {
            frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
    }
}

fn render_approval_dialog(frame: &mut Frame, area: Rect, info: &coder_agent::client::ToolCallInfo) {
    let w = (area.width * 6 / 10).max(40).min(area.width);
    let h = 7u16;
    let popup = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    let args_display = truncate_at_char_boundary(&info.arguments, 60);
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Tool: {}", info.name),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  Args: {}", args_display),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  [y] Approve    [n] Deny",
            Style::default().fg(Color::Cyan),
        )),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Approval Required")
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        popup,
    );
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
