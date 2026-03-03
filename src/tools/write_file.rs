use crate::tools::{Tool, ToolDefinition};
use serde_json::json;
use std::fs;
use std::path::Path;

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file",
            description: "Write content to a file at the given path, creating parent directories if needed",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to write to"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                },
                "required": ["path", "content"]
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

        let content = args
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| "Missing or invalid 'content' parameter".to_string())?;

        let file_path = Path::new(path);

        // Create parent directories if needed
        if let Some(parent) = file_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directories: {}", e))?;
            }
        }

        // Write the file
        fs::write(file_path, content)
            .map_err(|e| format!("Failed to write file '{}': {}", path, e))?;

        Ok(format!(
            "Successfully wrote {} bytes to '{}'",
            content.len(),
            path
        ))
    }
}
