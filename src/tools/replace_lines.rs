use crate::tools::{Tool, ToolDefinition};
use serde_json::{Value, json};
use std::fs;

pub struct ReplaceLinesTool;

impl Tool for ReplaceLinesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "replace_lines",
            description: "Replace a range of lines in a file with new content. \
                Use read_file first to see line numbers, then specify the inclusive \
                start_line and end_line to replace. To replace a single line, set \
                start_line and end_line to the same value. To delete lines, set \
                new_content to empty string.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to edit"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to replace (1-indexed, inclusive)"
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line to replace (1-indexed, inclusive)"
                    },
                    "new_content": {
                        "type": "string",
                        "description": "Replacement text. May contain newlines. Use empty string to delete the lines."
                    }
                },
                "required": ["path", "start_line", "end_line", "new_content"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {e}"))?;

        let path = args["path"]
            .as_str()
            .ok_or("Missing required parameter: path")?;
        let start = args["start_line"]
            .as_u64()
            .ok_or("Missing required parameter: start_line")? as usize;
        let end = args["end_line"]
            .as_u64()
            .ok_or("Missing required parameter: end_line")? as usize;
        let new_content = args["new_content"]
            .as_str()
            .ok_or("Missing required parameter: new_content")?;

        if start == 0 {
            return Err("start_line must be >= 1".to_string());
        }
        if end < start {
            return Err(format!("end_line ({end}) must be >= start_line ({start})"));
        }

        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read '{path}': {e}"))?;

        let mut lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let append_at_eof = start == total + 1 && end == total + 1;

        if start > total + 1 {
            return Err(format!(
                "start_line ({start}) is beyond end of file ({total} lines)"
            ));
        }
        if end > total && !append_at_eof {
            return Err(format!(
                "end_line ({end}) is beyond end of file ({total} lines)"
            ));
        }

        // Replace lines [start-1 .. end] with the new content lines.
        // Special case: when start == end and new_content is non-empty,
        // insert before start_line without removing any existing line.
        let replacement: Vec<&str> = if new_content.is_empty() {
            vec![]
        } else {
            new_content.lines().collect()
        };

        let range = if append_at_eof {
            total..total
        } else {
            (start - 1)..end
        };

        lines.splice(range, replacement);

        // Preserve trailing newline if original had one
        let mut result = lines.join("\n");
        if content.ends_with('\n') {
            result.push('\n');
        }

        fs::write(path, &result).map_err(|e| format!("Failed to write '{path}': {e}"))?;

        Ok(format!(
            "Replaced lines {start}–{end} in '{path}' ({total} → {} lines total).",
            lines.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn tool() -> ReplaceLinesTool {
        ReplaceLinesTool
    }

    /// Write content to a temp file and return it (kept alive by the caller).
    fn tmp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn args(path: &str, start: usize, end: usize, new_content: &str) -> String {
        serde_json::json!({
            "path": path,
            "start_line": start,
            "end_line": end,
            "new_content": new_content,
        })
        .to_string()
    }

    // ── happy-path ────────────────────────────────────────────────────────────

    #[test]
    fn replace_single_line() {
        let f = tmp("aaa\nbbb\nccc\n");
        let path = f.path().to_str().unwrap();
        let result = tool().execute(&args(path, 2, 2, "BBB")).unwrap();
        assert!(result.contains("27 →") || result.contains("→"), "{result}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "aaa\nBBB\nccc\n");
    }

    #[test]
    fn replace_range_of_lines() {
        let f = tmp("line1\nline2\nline3\nline4\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 2, 3, "X\nY")).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "line1\nX\nY\nline4\n"
        );
    }

    #[test]
    fn delete_lines() {
        let f = tmp("keep\ndelete_me\nalso_delete\nkeep2\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 2, 3, "")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep\nkeep2\n");
    }

    #[test]
    fn replace_first_line() {
        let f = tmp("old\nrest\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 1, 1, "new")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "new\nrest\n");
    }

    #[test]
    fn replace_last_line() {
        let f = tmp("first\nlast\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 2, 2, "replaced")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "first\nreplaced\n");
    }

    #[test]
    fn replace_expands_line_count() {
        // Replace 1 line with 3 lines.
        let f = tmp("a\nb\nc\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 2, 2, "x\ny\nz")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "a\nx\ny\nz\nc\n");
    }

    #[test]
    fn replace_shrinks_line_count() {
        // Replace 3 lines with 1 line.
        let f = tmp("a\nb\nc\nd\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 2, 4, "only")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "a\nonly\n");
    }

    #[test]
    fn append_at_eof() {
        let f = tmp("a\nb\n");
        let path = f.path().to_str().unwrap();
        // line 3 == total+1 → append
        tool().execute(&args(path, 3, 3, "c")).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn trailing_newline_preserved() {
        let f = tmp("a\nb\n");
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 1, 1, "A")).unwrap();
        assert!(
            std::fs::read_to_string(path).unwrap().ends_with('\n'),
            "trailing newline must be preserved"
        );
    }

    #[test]
    fn no_trailing_newline_not_added() {
        let f = tmp("a\nb"); // no trailing newline
        let path = f.path().to_str().unwrap();
        tool().execute(&args(path, 1, 1, "A")).unwrap();
        assert!(
            !std::fs::read_to_string(path).unwrap().ends_with('\n'),
            "trailing newline must not be added when original had none"
        );
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn error_start_zero() {
        let f = tmp("a\n");
        let path = f.path().to_str().unwrap();
        let err = tool().execute(&args(path, 0, 0, "x")).unwrap_err();
        assert!(err.contains("start_line must be >= 1"), "{err}");
    }

    #[test]
    fn error_end_before_start() {
        let f = tmp("a\nb\nc\n");
        let path = f.path().to_str().unwrap();
        let err = tool().execute(&args(path, 3, 1, "x")).unwrap_err();
        assert!(
            err.contains("end_line") && err.contains("start_line"),
            "{err}"
        );
    }

    #[test]
    fn error_start_beyond_eof() {
        let f = tmp("a\nb\n");
        let path = f.path().to_str().unwrap();
        let err = tool().execute(&args(path, 99, 99, "x")).unwrap_err();
        assert!(err.contains("beyond end of file"), "{err}");
    }

    #[test]
    fn error_end_beyond_eof() {
        let f = tmp("a\nb\n");
        let path = f.path().to_str().unwrap();
        let err = tool().execute(&args(path, 1, 99, "x")).unwrap_err();
        assert!(err.contains("beyond end of file"), "{err}");
    }

    #[test]
    fn error_missing_path() {
        let err = tool()
            .execute(r#"{"start_line":1,"end_line":1,"new_content":"x"}"#)
            .unwrap_err();
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn error_nonexistent_file() {
        let err = tool()
            .execute(&args("/nonexistent/path/file.txt", 1, 1, "x"))
            .unwrap_err();
        assert!(err.contains("Failed to read"), "{err}");
    }
}
