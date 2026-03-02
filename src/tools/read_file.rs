use crate::tools::{Tool, ToolDefinition};
use serde_json::json;
use std::fs;
use std::path::Path;

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file",
            description: "Read the contents of a file at the given path relative to the current working directory",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| "Missing or invalid 'path' parameter".to_string())?;

        // Ensure path exists and is a file
        let file_path = Path::new(path);
        if !file_path.exists() {
            return Err(format!("File not found: {}", path));
        }
        if !file_path.is_file() {
            return Err(format!("Path is not a file: {}", path));
        }

        // Read the file
        fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file '{}': {}", path, e))
    }
}
