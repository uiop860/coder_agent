use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, TextMergeStream};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug)]
enum StyleModifier {
    Bold,
    Italic,
    Link,
    None_,
}

#[derive(Clone, Copy, Debug)]
enum PrefixKind {
    Normal,
    Bullet,
    Blockquote,
}

struct MdRenderer {
    output: Vec<Line<'static>>,
    inline_buf: Vec<(String, Style)>,
    style_stack: Vec<StyleModifier>,
    in_code_block: bool,
    list_depth: u32,
    list_item_first_flushed: bool,
    heading_level: Option<HeadingLevel>,
    first_line_emitted: bool,
    text_width: usize,
    // Table state
    table_rows: Vec<(bool, Vec<Vec<(String, Style)>>)>, // (is_header, cells)
    table_current_cells: Vec<Vec<(String, Style)>>,
}

impl MdRenderer {
    fn new(text_width: usize) -> Self {
        Self {
            output: Vec::new(),
            inline_buf: Vec::new(),
            style_stack: Vec::new(),
            in_code_block: false,
            list_depth: 0,
            list_item_first_flushed: false,
            heading_level: None,
            first_line_emitted: false,
            text_width,
            table_rows: Vec::new(),
            table_current_cells: Vec::new(),
        }
    }

    fn effective_style(&self) -> Style {
        let mut style = Style::default();
        for modifier in &self.style_stack {
            match modifier {
                StyleModifier::Bold => {
                    style = style.add_modifier(Modifier::BOLD);
                }
                StyleModifier::Italic => {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                StyleModifier::Link => {
                    style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
                }
                StyleModifier::None_ => {}
            }
        }
        style
    }

    fn next_prefix(&mut self, kind: PrefixKind, is_first_bullet: bool) -> Span<'static> {
        match kind {
            PrefixKind::Normal => {
                if !self.first_line_emitted {
                    self.first_line_emitted = true;
                    Span::styled("● ", Style::default().fg(Color::White))
                } else {
                    Span::raw("  ")
                }
            }
            PrefixKind::Bullet => {
                if is_first_bullet {
                    let indent = "  ".repeat(self.list_depth.saturating_sub(1) as usize);
                    Span::styled(format!("{}• ", indent), Style::default().fg(Color::Green))
                } else {
                    let indent = "  ".repeat(self.list_depth as usize);
                    Span::raw(indent)
                }
            }
            PrefixKind::Blockquote => Span::styled("│ ", Style::default().fg(Color::Gray)),
        }
    }

    fn flush_inline(&mut self, block_style_override: Option<Style>, prefix_kind: PrefixKind) {
        if self.inline_buf.is_empty() {
            return;
        }

        let spans: Vec<(String, Style)> = self
            .inline_buf
            .drain(..)
            .map(|(text, style)| match block_style_override {
                Some(ov) => (text, merge_styles(style, ov)),
                None => (text, style),
            })
            .collect();

        let wrapped = wrap_spans(&spans, self.text_width);
        let is_first_bullet =
            matches!(prefix_kind, PrefixKind::Bullet) && !self.list_item_first_flushed;

        for (idx, line_spans) in wrapped.into_iter().enumerate() {
            let prefix = self.next_prefix(prefix_kind, idx == 0 && is_first_bullet);
            let mut span_vec = vec![prefix];
            for (text, style) in line_spans {
                span_vec.push(Span::styled(text, style));
            }
            self.output.push(Line::from(span_vec));
        }

        if matches!(prefix_kind, PrefixKind::Bullet) {
            self.list_item_first_flushed = true;
        }
    }

    fn emit_code_line(&mut self, line: &str) {
        let max_code_width = self.text_width.saturating_sub(2);
        let truncated = truncate_to_cols(line, max_code_width);
        let prefix = if !self.first_line_emitted {
            self.first_line_emitted = true;
            Span::styled("● ", Style::default().fg(Color::White))
        } else {
            Span::raw("  ")
        };
        self.output.push(Line::from(vec![
            prefix,
            Span::styled(truncated, Style::default().fg(Color::DarkGray)),
        ]));
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        let n_cols = self
            .table_rows
            .iter()
            .map(|(_, c)| c.len())
            .max()
            .unwrap_or(0);
        if n_cols == 0 {
            self.table_rows.clear();
            return;
        }

        // Natural column widths (max content display cols across all rows)
        let mut col_widths: Vec<usize> = vec![0; n_cols];
        for (_, cells) in &self.table_rows {
            for (i, cell) in cells.iter().enumerate() {
                if i < n_cols {
                    let w: usize = cell.iter().map(|(s, _)| s.width()).sum();
                    col_widths[i] = col_widths[i].max(w);
                }
            }
        }

        // Shrink columns proportionally if total exceeds available width
        // available = text_width - 2 (prefix) - (n_cols-1)*3 (separators " │ ")
        let sep_total = if n_cols > 1 { (n_cols - 1) * 3 } else { 0 };
        let available = self.text_width.saturating_sub(2 + sep_total);
        let natural_total: usize = col_widths.iter().sum();
        if natural_total > available && available > 0 {
            let ratio = available as f64 / natural_total as f64;
            for w in &mut col_widths {
                *w = ((*w as f64 * ratio).floor() as usize).max(3);
            }
        }

        let sep_style = Style::default().fg(Color::DarkGray);
        let header_base = Style::default().add_modifier(Modifier::BOLD);

        for (is_header, cells) in std::mem::take(&mut self.table_rows) {
            // Content row
            let prefix = if !self.first_line_emitted {
                self.first_line_emitted = true;
                Span::styled("● ", Style::default().fg(Color::White))
            } else {
                Span::raw("  ")
            };
            let mut span_vec: Vec<Span<'static>> = vec![prefix];

            for (col_idx, &col_w) in col_widths.iter().enumerate() {
                let empty = vec![];
                let cell_spans = cells.get(col_idx).unwrap_or(&empty);
                let cell_len: usize = cell_spans.iter().map(|(s, _)| s.width()).sum();

                let mut remaining = col_w;
                for (text, style) in cell_spans {
                    if remaining == 0 {
                        break;
                    }
                    let take = text.width().min(remaining);
                    let t = truncate_to_cols(text, take);
                    let s = if is_header {
                        merge_styles(*style, header_base)
                    } else {
                        *style
                    };
                    span_vec.push(Span::styled(t, s));
                    remaining = remaining.saturating_sub(take);
                }
                // Pad to col_w
                let padding = col_w.saturating_sub(cell_len);
                if padding > 0 {
                    span_vec.push(Span::raw(" ".repeat(padding)));
                }

                if col_idx < n_cols - 1 {
                    span_vec.push(Span::styled(" │ ", sep_style));
                }
            }
            self.output.push(Line::from(span_vec));

            // Separator after header
            if is_header {
                let mut sep_spans: Vec<Span<'static>> = vec![Span::raw("  ")];
                for (col_idx, &col_w) in col_widths.iter().enumerate() {
                    sep_spans.push(Span::styled("─".repeat(col_w), sep_style));
                    if col_idx < n_cols - 1 {
                        sep_spans.push(Span::styled("─┼─", sep_style));
                    }
                }
                self.output.push(Line::from(sep_spans));
            }
        }

        self.output.push(Line::from(Span::raw("")));
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::Start(Tag::Paragraph) => {
                self.style_stack.push(StyleModifier::None_);
            }
            Event::End(TagEnd::Paragraph) => {
                self.style_stack.pop();
                self.flush_inline(None, PrefixKind::Normal);
                self.output.push(Line::from(Span::raw("")));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                self.heading_level = Some(level);
                self.style_stack.push(StyleModifier::None_);
            }
            Event::End(TagEnd::Heading(_)) => {
                self.style_stack.pop();
                let level = self.heading_level.take();
                let heading_style = match level {
                    Some(HeadingLevel::H1) => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Some(HeadingLevel::H2) => Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                };
                self.flush_inline(Some(heading_style), PrefixKind::Normal);
                self.output.push(Line::from(Span::raw("")));
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                self.output.push(Line::from(Span::raw("")));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.style_stack.push(StyleModifier::None_);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.style_stack.pop();
                let blockquote_style = Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC);
                self.flush_inline(Some(blockquote_style), PrefixKind::Blockquote);
                self.output.push(Line::from(Span::raw("")));
            }
            Event::Start(Tag::List(_)) => {
                self.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                if self.list_depth == 0 {
                    self.output.push(Line::from(Span::raw("")));
                }
            }
            Event::Start(Tag::Item) => {
                self.list_item_first_flushed = false;
                self.style_stack.push(StyleModifier::None_);
            }
            Event::End(TagEnd::Item) => {
                self.flush_inline(None, PrefixKind::Bullet);
                self.style_stack.pop();
            }
            Event::Start(Tag::Strong) => {
                self.style_stack.push(StyleModifier::Bold);
            }
            Event::End(TagEnd::Strong) => {
                self.style_stack.pop();
            }
            Event::Start(Tag::Emphasis) => {
                self.style_stack.push(StyleModifier::Italic);
            }
            Event::End(TagEnd::Emphasis) => {
                self.style_stack.pop();
            }
            Event::Start(Tag::Link { .. }) => {
                self.style_stack.push(StyleModifier::Link);
            }
            Event::End(TagEnd::Link) => {
                self.style_stack.pop();
            }
            // ── Table events ────────────────────────────────────────────────
            // Note: TableHead contains TableCell directly (no TableRow wrapper).
            //       TableRow is only emitted for body rows.
            Event::Start(Tag::Table(_)) => {
                self.table_rows.clear();
                self.table_current_cells.clear();
            }
            Event::End(TagEnd::Table) => {
                self.render_table();
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                self.table_current_cells.clear();
            }
            Event::End(TagEnd::TableHead) => {
                // Header cells land directly here with no TableRow wrapper
                if !self.table_current_cells.is_empty() {
                    let cells = std::mem::take(&mut self.table_current_cells);
                    self.table_rows.push((true, cells));
                }
            }
            Event::End(TagEnd::TableRow) => {
                if !self.table_current_cells.is_empty() {
                    let cells = std::mem::take(&mut self.table_current_cells);
                    self.table_rows.push((false, cells));
                }
            }
            Event::End(TagEnd::TableCell) => {
                let cell = std::mem::take(&mut self.inline_buf);
                self.table_current_cells.push(cell);
            }
            // ── Inline content ───────────────────────────────────────────────
            Event::Text(s) => {
                if self.in_code_block {
                    for line in s.lines() {
                        self.emit_code_line(line);
                    }
                } else {
                    let style = self.effective_style();
                    self.inline_buf.push((s.into_string(), style));
                }
            }
            Event::Code(s) => {
                self.inline_buf
                    .push((s.into_string(), Style::default().fg(Color::Cyan)));
            }
            Event::SoftBreak => {
                let style = self.effective_style();
                self.inline_buf.push((" ".to_string(), style));
            }
            Event::HardBreak => {
                self.flush_inline(None, PrefixKind::Normal);
            }
            Event::Rule => {
                let dashes = "─".repeat(self.text_width);
                self.output.push(Line::from(Span::styled(
                    dashes,
                    Style::default().fg(Color::DarkGray),
                )));
                self.output.push(Line::from(Span::raw("")));
            }
            _ => {}
        }
    }
}

fn merge_styles(base: Style, override_style: Style) -> Style {
    let mut result = override_style;
    if base.add_modifier.contains(Modifier::BOLD) {
        result = result.add_modifier(Modifier::BOLD);
    }
    if base.add_modifier.contains(Modifier::ITALIC) {
        result = result.add_modifier(Modifier::ITALIC);
    }
    result
}

fn wrap_spans(spans: &[(String, Style)], width: usize) -> Vec<Vec<(String, Style)>> {
    if width == 0 {
        return vec![];
    }

    let mut lines: Vec<Vec<(String, Style)>> = vec![vec![]];
    let mut current_col: usize = 0;

    for (text, style) in spans {
        let mut tokens: Vec<&str> = Vec::new();
        let mut last = 0;
        for (i, c) in text.char_indices() {
            if c == ' ' {
                if last < i {
                    tokens.push(&text[last..i]);
                }
                tokens.push(" ");
                last = i + 1;
            }
        }
        if last < text.len() {
            tokens.push(&text[last..]);
        }

        for token in tokens {
            let token_len = token.width();

            if token == " " {
                if current_col > 0 && current_col < width {
                    if let Some(last_line) = lines.last_mut() {
                        last_line.push((token.to_string(), *style));
                    }
                    current_col += 1;
                }
                continue;
            }

            if current_col + token_len > width && current_col > 0 {
                lines.push(vec![]);
                current_col = 0;
            }

            if let Some(last_line) = lines.last_mut() {
                last_line.push((token.to_string(), *style));
            }
            current_col += token_len;
        }
    }

    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines
}

fn truncate_to_cols(s: &str, max_cols: usize) -> String {
    let mut width = 0;
    for (byte_idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_cols {
            return s[..byte_idx].to_string();
        }
        width += ch_width;
    }
    s.to_string()
}

pub fn render_markdown(content: &str, text_width: usize) -> Vec<Line<'static>> {
    if content.is_empty() {
        return vec![];
    }

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = TextMergeStream::new(Parser::new_ext(content, opts));
    let mut renderer = MdRenderer::new(text_width);

    for event in parser {
        renderer.process_event(event);
    }

    if !renderer.inline_buf.is_empty() {
        renderer.flush_inline(None, PrefixKind::Normal);
    }

    while renderer
        .output
        .last()
        .map(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
        .unwrap_or(false)
    {
        renderer.output.pop();
    }

    renderer.output
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    fn plain_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_text_wraps() {
        let lines = render_markdown("hello world foo bar baz", 10);
        assert!(
            lines.len() >= 2,
            "expected wrapping, got {} lines",
            lines.len()
        );
    }

    #[test]
    fn bold_applies_modifier() {
        let lines = render_markdown("**bold**", 80);
        let has_bold = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        });
        assert!(has_bold, "expected bold modifier in output");
    }

    #[test]
    fn italic_applies_modifier() {
        let lines = render_markdown("*italic*", 80);
        let has_italic = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
        });
        assert!(has_italic, "expected italic modifier in output");
    }

    #[test]
    fn inline_code_is_cyan() {
        let lines = render_markdown("use `code` here", 80);
        let has_cyan = lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.style.fg == Some(Color::Cyan)));
        assert!(has_cyan, "expected cyan color for inline code");
    }

    #[test]
    fn code_block_not_wrapped() {
        let content = "```\nshort\n```";
        let lines = render_markdown(content, 10);
        assert!(!lines.is_empty());
    }

    #[test]
    fn empty_input_returns_empty() {
        let lines = render_markdown("", 80);
        assert!(lines.is_empty());
    }

    #[test]
    fn horizontal_rule_emits_dashes() {
        let lines = render_markdown("---\n", 10);
        let has_dashes = lines.iter().any(|line| plain_text(line).contains('─'));
        assert!(has_dashes, "expected dashes for horizontal rule");
    }

    #[test]
    fn heading_h1_is_bold_cyan() {
        let lines = render_markdown("# Title\n", 80);
        let has_bold_cyan = lines.iter().any(|line| {
            line.spans.iter().any(|s| {
                s.style.fg == Some(Color::Cyan) && s.style.add_modifier.contains(Modifier::BOLD)
            })
        });
        assert!(has_bold_cyan, "expected bold cyan for H1 heading");
    }

    #[test]
    fn table_renders_with_separator() {
        let md = "| A | B |\n|---|---|\n| x | y |\n";
        let lines = render_markdown(md, 40);
        // Should have: header row, separator row, data row, blank
        assert!(lines.len() >= 3, "expected at least 3 lines for table");
        let sep_line = lines.iter().any(|l| plain_text(l).contains('─'));
        assert!(sep_line, "expected separator line in table");
    }

    #[test]
    fn table_header_is_bold() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | bar   |\n";
        let lines = render_markdown(md, 40);
        // First content row should have a bold span
        let has_bold = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        });
        assert!(has_bold, "expected bold header in table");
    }

    #[test]
    fn table_wide_chars_aligned() {
        // Emoji are 2 terminal columns wide. Every content row should render to
        // the same display width so that columns are visually aligned.
        let md = "| Feature    | Support |\n|------------|--------|\n| Tables     | ✅      |\n| Task Lists | ❌      |\n";
        let lines = render_markdown(md, 60);

        // Collect display widths of all non-empty, non-separator content rows
        // (skip the blank trailing line and the ─┼─ separator line).
        let row_widths: Vec<usize> = lines
            .iter()
            .filter(|l| {
                let t = plain_text(l);
                !t.trim().is_empty() && !t.trim_start().starts_with('─')
            })
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum::<usize>()
            })
            .collect();

        assert!(row_widths.len() >= 3, "expected header + 2 data rows");
        let first = row_widths[0];
        for (i, &w) in row_widths.iter().enumerate() {
            assert_eq!(
                w, first,
                "row {} has display width {} but expected {} — emoji misaligned",
                i, w, first
            );
        }
    }
}
