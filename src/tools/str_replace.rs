use crate::tools::{Tool, ToolDefinition};
use serde_json::{Value, json};
use std::fs;

pub struct StrReplaceTool;

impl Tool for StrReplaceTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "str_replace",
            description: "Replace an exact, unique string in a file. Fails if the string is not found or appears more than once.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to edit"
                    },
                    "old_str": {
                        "type": "string",
                        "description": "Exact text to find and replace. Must be unique in the file."
                    },
                    "new_str": {
                        "type": "string",
                        "description": "Replacement text. Use empty string to delete old_str."
                    }
                },
                "required": ["path", "old_str", "new_str"]
            }),
        }
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {e}"))?;

        let path = args["path"]
            .as_str()
            .ok_or("Missing required parameter: path")?;
        let old_str = args["old_str"]
            .as_str()
            .ok_or("Missing required parameter: old_str")?;
        let new_str = args["new_str"]
            .as_str()
            .ok_or("Missing required parameter: new_str")?;

        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read '{path}': {e}"))?;

        let count = content.matches(old_str).count();

        if count == 0 {
            return Err(format!(
                "str_replace failed: 'old_str' not found in '{path}'. Verify exact whitespace and content."
            ));
        }
        if count >= 2 {
            return Err(format!(
                "str_replace failed: 'old_str' matches {count} locations in '{path}'. Add more surrounding context to make it unique."
            ));
        }

        let offset = content.find(old_str).unwrap();
        let line_number = content[..offset].chars().filter(|&c| c == '\n').count() + 1;

        let new_content = content.replacen(old_str, new_str, 1);

        fs::write(path, new_content).map_err(|e| format!("Failed to write '{path}': {e}"))?;

        Ok(format!(
            "Replaced at line {line_number} in '{path}'. Verify with read_file if needed."
        ))
    }
}
