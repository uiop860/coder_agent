use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::client::ToolCallInfo;

/// Compute a compact plain-text diff for an `edit_file` tool call.
/// Lines are prefixed with `+ ` (insert), `- ` (delete), or `  ` (context).
/// Returns None if args cannot be parsed.
pub fn compute_replace_diff_text(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let old_chunk = v.get("old_string")?.as_str()?;
    let new_content = v.get("new_string")?.as_str()?;

    let diff = TextDiff::from_lines(old_chunk, new_content);

    let rows: Vec<(ChangeTag, String)> = diff
        .iter_all_changes()
        .map(|c| (c.tag(), c.value().trim_end_matches('\n').to_string()))
        .collect();

    let change_positions: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, (tag, _))| *tag != ChangeTag::Equal)
        .map(|(i, _)| i)
        .collect();

    if change_positions.is_empty() {
        return Some("  (no changes)".to_string());
    }

    let first = *change_positions.first().unwrap();
    let last_idx = *change_positions.last().unwrap();
    let keep_start = first.saturating_sub(2);
    let keep_end = (last_idx + 2).min(rows.len().saturating_sub(1));

    let mut out = String::new();
    if keep_start > 0 {
        out.push_str(&format!("  … {} lines\n", keep_start));
    }
    for (tag, text) in &rows[keep_start..=keep_end] {
        let prefix = match tag {
            ChangeTag::Delete => "- ",
            ChangeTag::Insert => "+ ",
            ChangeTag::Equal => "  ",
        };
        out.push_str(prefix);
        out.push_str(text);
        out.push('\n');
    }
    let skipped_after = rows.len().saturating_sub(1).saturating_sub(keep_end);
    if skipped_after > 0 {
        out.push_str(&format!("  … {} lines\n", skipped_after));
    }
    if out.ends_with('\n') {
        out.pop();
    }
    Some(out)
}

/// Build a coloured before/after diff preview for `write_file` and
/// `edit_file` tool calls.  Returns an empty vec for all other tools,
/// causing the approval modal to fall back to its plain "Args:" display.
pub fn compute_diff_preview(info: &ToolCallInfo, max_lines: usize) -> Vec<Line<'static>> {
    match info.name.as_str() {
        "write_file" => diff_write_file(&info.arguments, max_lines),
        "edit_file" => diff_edit_file(&info.arguments, max_lines),
        _ => vec![],
    }
}

fn diff_write_file(args: &str, max_lines: usize) -> Vec<Line<'static>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return vec![];
    };
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let new_content = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let old_content = std::fs::read_to_string(&path).unwrap_or_default();
    build_diff_lines(&old_content, &new_content, None, max_lines)
}

fn diff_edit_file(args: &str, max_lines: usize) -> Vec<Line<'static>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return vec![];
    };
    let file_path = v
        .get("file_path")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let old_string = v
        .get("old_string")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let new_string = v
        .get("new_string")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let header = format!("  {}", file_path);
    let mut lines = vec![Line::from(Span::styled(
        header,
        Style::default().fg(Color::DarkGray),
    ))];
    lines.extend(build_diff_lines(
        &old_string,
        &new_string,
        None,
        max_lines.saturating_sub(1),
    ));
    lines
}

/// Core diff renderer. Produces coloured `Line` values from two text chunks.
/// Context lines: at most 2 before first change, 2 after last change.
fn build_diff_lines(
    old: &str,
    new: &str,
    _header: Option<&str>,
    max_lines: usize,
) -> Vec<Line<'static>> {
    let diff = TextDiff::from_lines(old, new);

    // Collect all ops with their change info so we can apply context trimming.
    #[derive(Clone)]
    struct DiffRow {
        tag: ChangeTag,
        text: String,
    }

    let mut rows: Vec<DiffRow> = Vec::new();
    for change in diff.iter_all_changes() {
        let tag = change.tag();
        let text = change.value().trim_end_matches('\n').to_string();
        rows.push(DiffRow { tag, text });
    }

    // Determine which equal lines to keep (2 context around changes).
    let change_positions: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.tag != ChangeTag::Equal)
        .map(|(i, _)| i)
        .collect();

    if change_positions.is_empty() {
        // No changes — show a short "no changes" note.
        return vec![Line::from(Span::styled(
            "  (no changes)",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let first_change = *change_positions.first().unwrap();
    let last_change = *change_positions.last().unwrap();
    // Context window: 2 lines before first change, 2 lines after last change.
    let keep_start = first_change.saturating_sub(2);
    let keep_end = (last_change + 2).min(rows.len().saturating_sub(1));

    let mut visible: Vec<Line<'static>> = Vec::new();
    let mut skipped_before = 0usize;

    if keep_start > 0 {
        skipped_before = keep_start;
    }
    if skipped_before > 0 {
        visible.push(Line::from(Span::styled(
            format!("  … {} lines", skipped_before),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    for row in &rows[keep_start..=keep_end] {
        let (prefix, color) = match row.tag {
            ChangeTag::Delete => ("- ", Color::Red),
            ChangeTag::Insert => ("+ ", Color::Green),
            ChangeTag::Equal => ("  ", Color::DarkGray),
        };
        let display = format!("{}{}", prefix, row.text);
        visible.push(Line::from(Span::styled(
            display,
            Style::default().fg(color),
        )));
    }

    let after_end = rows.len().saturating_sub(1);
    let skipped_after = after_end.saturating_sub(keep_end);
    if skipped_after > 0 {
        visible.push(Line::from(Span::styled(
            format!("  … {} lines", skipped_after),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    // Cap to max_lines, adding a "… N more lines" footer if needed.
    if visible.len() > max_lines {
        let extra = visible.len() - max_lines + 1; // +1 for the footer line itself
        visible.truncate(max_lines - 1);
        visible.push(Line::from(Span::styled(
            format!("  … {} more lines", extra),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    visible
}
