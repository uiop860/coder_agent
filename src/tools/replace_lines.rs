use crate::tools::{Tool, ToolDefinition};
use log::debug;
use serde_json::{Value, json};
use std::fs;

pub struct EditFileTool;

impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file",
            description: r#"Performs exact string replacements in files.

Usage:
- When editing text from read_file output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: line number + colon + space (e.g., `1: `). Everything after that space is the actual file content to match. Never include any part of the line number prefix in the old_string or new_string.
- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.
- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked.
- The edit will FAIL if `old_string` is not found in the file with an error "oldString not found in content".
- The edit will FAIL if `old_string` is found multiple times in the file with an error "Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match." Either provide a larger string with more surrounding context to make it unique or use `replace_all` to change every instance of `old_string`.
- Use `replace_all` for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance.
- Set old_string to empty string "" to create or overwrite the file entirely with new_string."#,
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The text to find and replace. Must match the file content exactly \
                            (including indentation). Use empty string to create/overwrite the file."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text. Use empty string to delete old_string."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "If true, replace all occurrences of old_string. Defaults to false."
                    }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {e}"))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or("Missing required parameter: file_path")?;
        let old_string = args["old_string"]
            .as_str()
            .ok_or("Missing required parameter: old_string")?;
        let new_string = args["new_string"]
            .as_str()
            .ok_or("Missing required parameter: new_string")?;
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        if old_string == new_string {
            return Err(
                "old_string and new_string are identical — no changes would be made.".to_string(),
            );
        }

        if old_string.is_empty() {
            // Create or overwrite the file entirely.
            if let Some(parent) = std::path::Path::new(file_path).parent()
                && !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!("Failed to create directories for '{file_path}': {e}")
                    })?;
                }
            fs::write(file_path, new_string)
                .map_err(|e| format!("Failed to write '{file_path}': {e}"))?;
            return Ok(format!("Created/overwrote '{file_path}'."));
        }

        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read '{file_path}': {e}"))?;

        debug!(
            "edit_file: file='{}' content_len={} old_len={} replace_all={}",
            file_path,
            content.len(),
            old_string.len(),
            replace_all
        );
        debug!(
            "edit_file: old_string bytes={:?}",
            old_string.as_bytes().iter().take(200).collect::<Vec<_>>()
        );
        debug!(
            "edit_file: file first 500 bytes={:?}",
            content.as_bytes().iter().take(500).collect::<Vec<_>>()
        );

        let result = replace(&content, old_string, new_string, replace_all)
            .map_err(|e| format!("{e}\n\nCurrent file content:\n{content}"))?;

        fs::write(file_path, &result).map_err(|e| format!("Failed to write '{file_path}': {e}"))?;

        Ok(format!("Edited '{file_path}' successfully."))
    }
}

// ── Levenshtein distance ──────────────────────────────────────────────────────

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])
            };
        }
    }
    dp[m][n]
}

fn similarity(a: &str, b: &str) -> f64 {
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f64 / max_len as f64
}

// ── Replacers ─────────────────────────────────────────────────────────────────
// Each returns candidate strings to search for in content.

fn simple_replacer(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

/// Match lines when trimmed; yield the original substring from content.
fn line_trimmed_replacer(content: &str, find: &str) -> Vec<String> {
    let find_lines: Vec<&str> = find.lines().collect();
    if find_lines.is_empty() {
        return vec![];
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = find_lines.len();
    let mut candidates = Vec::new();

    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        for (j, fl) in find_lines.iter().enumerate() {
            if content_lines[i + j].trim() != fl.trim() {
                continue 'outer;
            }
        }
        // Build the actual substring from content.
        let chunk: String = content_lines[i..i + n].join("\n");
        candidates.push(chunk);
    }
    candidates
}

/// Use first/last lines as anchors; middle lines matched via Levenshtein similarity.
/// Requires ≥3 search lines.
fn block_anchor_replacer(content: &str, find: &str) -> Vec<String> {
    let find_lines: Vec<&str> = find.lines().collect();
    if find_lines.len() < 3 {
        return vec![];
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = find_lines.len();
    let threshold = if n == 1 { 0.0 } else { 0.3 };
    let mut candidates = Vec::new();

    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        // Anchor first and last lines exactly.
        if content_lines[i].trim() != find_lines[0].trim() {
            continue;
        }
        if content_lines[i + n - 1].trim() != find_lines[n - 1].trim() {
            continue;
        }
        // Check middle lines via similarity.
        for j in 1..n - 1 {
            if similarity(content_lines[i + j], find_lines[j]) < threshold {
                continue 'outer;
            }
        }
        let chunk: String = content_lines[i..i + n].join("\n");
        candidates.push(chunk);
    }
    candidates
}

/// Collapse whitespace runs to single space before comparing.
fn whitespace_normalized_replacer(content: &str, find: &str) -> Vec<String> {
    fn normalize(s: &str) -> String {
        let re_like: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        re_like
    }

    let norm_find = normalize(find);
    // Try whole-content match first (single line style).
    let norm_content = normalize(content);
    if let Some(idx) = norm_content.find(&norm_find) {
        // Map back to original — heuristic: return find lines trimmed from content.
        let _ = idx;
    }

    // Line-by-line approach: slide over content lines.
    let find_lines: Vec<&str> = find.lines().collect();
    if find_lines.is_empty() {
        return vec![];
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = find_lines.len();
    let mut candidates = Vec::new();

    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        for (j, fl) in find_lines.iter().enumerate() {
            if normalize(content_lines[i + j]) != normalize(fl) {
                continue 'outer;
            }
        }
        let chunk: String = content_lines[i..i + n].join("\n");
        candidates.push(chunk);
    }
    candidates
}

/// Strip minimum indentation from both sides and compare.
fn indentation_flexible_replacer(content: &str, find: &str) -> Vec<String> {
    fn min_indent(lines: &[&str]) -> usize {
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0)
    }

    fn strip_indent(lines: &[&str], n: usize) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                if l.len() >= n {
                    l[n..].to_string()
                } else {
                    l.trim_start().to_string()
                }
            })
            .collect()
    }

    let find_lines: Vec<&str> = find.lines().collect();
    if find_lines.is_empty() {
        return vec![];
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = find_lines.len();
    let find_indent = min_indent(&find_lines);
    let stripped_find = strip_indent(&find_lines, find_indent);
    let mut candidates = Vec::new();

    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        let chunk_lines = &content_lines[i..i + n];
        let chunk_indent = min_indent(chunk_lines);
        let stripped_chunk = strip_indent(chunk_lines, chunk_indent);
        for (j, sf) in stripped_find.iter().enumerate() {
            if stripped_chunk[j] != *sf {
                continue 'outer;
            }
        }
        let chunk: String = chunk_lines.join("\n");
        candidates.push(chunk);
    }
    candidates
}

/// Unescape common escape sequences before comparing.
fn escape_normalized_replacer(content: &str, find: &str) -> Vec<String> {
    fn unescape(s: &str) -> String {
        s.replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\r", "\r")
            .replace("\\\\", "\\")
            .replace("\\\"", "\"")
            .replace("\\'", "'")
            .replace("\\`", "`")
    }

    let unescaped = unescape(find);
    if unescaped == find {
        return vec![];
    }
    // Recurse with the unescaped version via simple_replacer.
    simple_replacer(content, &unescaped)
}

/// Try find.trim() if it differs from find.
fn trimmed_boundary_replacer(content: &str, find: &str) -> Vec<String> {
    let trimmed = find.trim();
    if trimmed == find {
        return vec![];
    }
    simple_replacer(content, trimmed)
}

/// Anchor on first/last lines; require ≥50% of middle lines to match.
/// Requires ≥3 search lines.
fn context_aware_replacer(content: &str, find: &str) -> Vec<String> {
    let find_lines: Vec<&str> = find.lines().collect();
    if find_lines.len() < 3 {
        return vec![];
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = find_lines.len();
    let mut candidates = Vec::new();

    for i in 0..content_lines.len().saturating_sub(n - 1) {
        if content_lines[i].trim() != find_lines[0].trim() {
            continue;
        }
        if content_lines[i + n - 1].trim() != find_lines[n - 1].trim() {
            continue;
        }
        let middle_matches = (1..n - 1)
            .filter(|&j| content_lines[i + j].trim() == find_lines[j].trim())
            .count();
        let middle_total = n - 2;
        if middle_total == 0 || middle_matches * 2 >= middle_total {
            let chunk: String = content_lines[i..i + n].join("\n");
            candidates.push(chunk);
        }
    }
    candidates
}

/// Yield find for every occurrence (used by replace_all path).
fn multi_occurrence_replacer(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

// ── Core replace logic ────────────────────────────────────────────────────────

fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    type Replacer = fn(&str, &str) -> Vec<String>;
    let replacers: &[(&str, Replacer)] = &[
        ("simple", simple_replacer),
        ("line_trimmed", line_trimmed_replacer),
        ("block_anchor", block_anchor_replacer),
        ("whitespace_normalized", whitespace_normalized_replacer),
        ("indentation_flexible", indentation_flexible_replacer),
        ("escape_normalized", escape_normalized_replacer),
        ("trimmed_boundary", trimmed_boundary_replacer),
        ("context_aware", context_aware_replacer),
        ("multi_occurrence", multi_occurrence_replacer),
    ];

    let mut multiple_matches = false;

    for (name, replacer) in replacers {
        let candidates = replacer(content, old);
        debug!(
            "edit_file: replacer='{}' produced {} candidate(s)",
            name,
            candidates.len()
        );
        for search in candidates {
            let Some(first) = content.find(&*search) else {
                debug!(
                    "edit_file: replacer='{}' candidate not found in content",
                    name
                );
                continue;
            };
            // At least one match found.
            if replace_all {
                debug!("edit_file: replacer='{}' matched (replace_all)", name);
                return Ok(content.replace(&*search, new));
            }
            let last = content.rfind(&*search).unwrap();
            if first != last {
                // Multiple matches for this candidate — remember but keep trying.
                debug!(
                    "edit_file: replacer='{}' found multiple matches at {} and {}",
                    name, first, last
                );
                multiple_matches = true;
                continue;
            }
            debug!("edit_file: replacer='{}' matched at offset {}", name, first);
            return Ok(format!(
                "{}{}{}",
                &content[..first],
                new,
                &content[first + search.len()..]
            ));
        }
    }

    if multiple_matches {
        Err("Found multiple matches for old_string. Provide more surrounding context to make the match unique.".to_string())
    } else {
        Err("Could not find old_string in the file. It must match exactly, including whitespace, indentation, and line endings.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn tool() -> EditFileTool {
        EditFileTool
    }

    fn tmp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn args(path: &str, old: &str, new: &str) -> String {
        serde_json::json!({
            "file_path": path,
            "old_string": old,
            "new_string": new,
        })
        .to_string()
    }

    fn args_all(path: &str, old: &str, new: &str) -> String {
        serde_json::json!({
            "file_path": path,
            "old_string": old,
            "new_string": new,
            "replace_all": true,
        })
        .to_string()
    }

    // ── happy-path ────────────────────────────────────────────────────────────

    #[test]
    fn replace_single_line() {
        let f = tmp("aaa\nbbb\nccc\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, "bbb", "BBB")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "aaa\nBBB\nccc\n");
    }

    #[test]
    fn replace_multiline_block() {
        let f = tmp("line1\nline2\nline3\nline4\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, "line2\nline3", "X\nY")).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "line1\nX\nY\nline4\n"
        );
    }

    #[test]
    fn delete_text() {
        let f = tmp("keep\ndelete_me\nkeep2\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, "\ndelete_me", "")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep\nkeep2\n");
    }

    #[test]
    fn create_overwrite_with_empty_old() {
        let f = tmp("old content\n");
        let path = f.path().to_str().unwrap();
        tool()
            .execute(&args(path, "", "brand new content\n"))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "brand new content\n"
        );
    }

    #[test]
    fn replace_all_occurrences() {
        let f = tmp("foo bar foo baz foo\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args_all(path, "foo", "qux")).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "qux bar qux baz qux\n"
        );
    }

    // ── fuzzy matching ────────────────────────────────────────────────────────

    #[test]
    fn trim_matching() {
        // find has surrounding whitespace, content has none
        let result = replace("hello world", "  hello world  ", "goodbye", false).unwrap();
        assert_eq!(result, "goodbye");
    }

    #[test]
    fn line_trimmed_matching() {
        let content = "    fn foo() {\n        bar();\n    }\n";
        let find = "fn foo() {\n    bar();\n}\n";
        // indentation_flexible_replacer or line_trimmed_replacer should match
        let result = replace(content, find, "fn foo() { baz(); }", false).unwrap();
        assert!(result.contains("fn foo() { baz(); }"), "{result}");
    }

    #[test]
    fn whitespace_normalized_matching() {
        let content = "let  x  =  1;";
        let find = "let x = 1;";
        let result = replace(content, find, "let x = 2;", false).unwrap();
        assert_eq!(result, "let x = 2;");
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn error_not_found() {
        let err = replace("hello world", "nonexistent", "x", false).unwrap_err();
        assert!(err.contains("Could not find"), "{err}");
    }

    #[test]
    fn error_multiple_matches() {
        let err = replace("foo foo", "foo", "bar", false).unwrap_err();
        assert!(err.contains("multiple matches"), "{err}");
    }

    #[test]
    fn error_identical_strings() {
        let f = tmp("abc\n");
        let path = f.path().to_str().unwrap();
        let err = tool().execute(&args(path, "abc", "abc")).unwrap_err();
        assert!(err.contains("identical"), "{err}");
    }

    #[test]
    fn error_missing_file_path() {
        let err = tool()
            .execute(r#"{"old_string":"x","new_string":"y"}"#)
            .unwrap_err();
        assert!(err.contains("file_path"), "{err}");
    }

    #[test]
    fn error_nonexistent_file() {
        let err = tool()
            .execute(&args("/nonexistent/path/file.txt", "x", "y"))
            .unwrap_err();
        assert!(err.contains("Failed to read"), "{err}");
    }
}
