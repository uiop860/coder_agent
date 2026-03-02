use crate::tools::{Tool, ToolDefinition};
use serde_json::json;
use std::fs;
use std::path::Path;

pub struct SearchFileTool;

impl Tool for SearchFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_for_file",
            description: "Recursively search for files whose name contains the given pattern. Returns up to 50 results.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The pattern to search for in file names"
                    },
                    "root": {
                        "type": "string",
                        "description": "The root directory to search in (defaults to current directory)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let pattern = args
            .get("pattern")
            .and_then(|p| p.as_str())
            .ok_or_else(|| "Missing or invalid 'pattern' parameter".to_string())?;

        let root_str = args
            .get("root")
            .and_then(|r| r.as_str())
            .unwrap_or(".");

        let root = Path::new(root_str);

        // Ensure root exists and is a directory
        if !root.exists() {
            return Err(format!("Directory not found: {}", root_str));
        }
        if !root.is_dir() {
            return Err(format!("Path is not a directory: {}", root_str));
        }

        let mut results: Vec<String> = Vec::new();
        search_recursive(root, pattern, &mut results, 50)?;

        if results.is_empty() {
            Ok("No files found matching pattern".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

fn search_recursive(
    dir: &Path,
    pattern: &str,
    results: &mut Vec<String>,
    limit: usize,
) -> Result<(), String> {
    // Early exit if we've hit the limit
    if results.len() >= limit {
        return Ok(());
    }

    let entries = fs::read_dir(dir).map_err(|e| format!("Failed to read directory: {}", e))?;

    for entry in entries {
        if results.len() >= limit {
            break;
        }

        let entry = entry.map_err(|e| format!("Error reading entry: {}", e))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        if file_name_str.contains(pattern) {
            let path_str = path.to_string_lossy().to_string();
            results.push(path_str);
        }

        // Recurse into directories
        if path.is_dir() {
            search_recursive(&path, pattern, results, limit)?;
        }
    }

    Ok(())
}
