use crate::tools::{Tool, ToolDefinition};
use serde_json::json;

pub struct CurrentDirTool;

impl Tool for CurrentDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_current_directory",
            description: "Get the current working directory as an absolute path",
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn execute(&self, _arguments: &str) -> Result<String, String> {
        std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))
            .map(|p| p.to_string_lossy().to_string())
    }
}
