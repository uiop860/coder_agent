use crate::tools::{Tool, ToolDefinition};
use serde_json::json;
use std::fs;
use std::path::Path;

pub struct ListDirTool;

impl Tool for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_directory",
            description: "List all entries in a directory, one per line, with directories marked by a trailing /",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The directory path to list (defaults to current directory if empty)"
                    }
                },
                "required": []
            }),
        }
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let path_str = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");

        let dir_path = Path::new(path_str);

        // Ensure path exists and is a directory
        if !dir_path.exists() {
            return Err(format!("Directory not found: {}", path_str));
        }
        if !dir_path.is_dir() {
            return Err(format!("Path is not a directory: {}", path_str));
        }

        // List entries
        let mut entries: Vec<String> = Vec::new();
        let read_dir = fs::read_dir(dir_path)
            .map_err(|e| format!("Failed to read directory '{}': {}", path_str, e))?;

        for entry in read_dir {
            let entry = entry.map_err(|e| format!("Error reading entry: {}", e))?;
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy().to_string();
            let entry_path = entry.path();
            let is_dir = entry_path.is_dir();
            if is_dir {
                entries.push(format!("{}/", file_name_str));
            } else {
                entries.push(file_name_str);
            }
        }

        entries.sort();
        Ok(entries.join("\n"))
    }
}
