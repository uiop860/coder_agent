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
                start_line and end_line to replace. To insert without removing lines, \
                set start_line and end_line to the same line number of the line you \
                want to insert before. To delete lines, set new_content to empty string.",
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
        } else if start == end && !replacement.is_empty() {
            (start - 1)..(start - 1)
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
