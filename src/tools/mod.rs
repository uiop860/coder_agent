use serde_json::Value;
use std::sync::Arc;

pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value, // JSON Schema object for the tool's arguments
}

/// Implemented by every tool. Execution is synchronous (local I/O only).
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    /// `arguments` is a JSON string matching the tool's `parameters` schema.
    /// Returns plain-text (or JSON) output that is fed back to the model.
    fn execute(&self, arguments: &str) -> Result<String, String>;
}

/// Convenience: build the default set of codebase tools.
pub fn default_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFileTool),
        Arc::new(write_file::WriteFileTool),
        Arc::new(list_dir::ListDirTool),
        Arc::new(search_file::SearchFileTool),
        Arc::new(current_dir::CurrentDirTool),
        Arc::new(replace_lines::ReplaceLinesTool),
    ]
}

pub mod read_file;
pub mod write_file;
pub mod list_dir;
pub mod search_file;
pub mod current_dir;
pub mod str_replace;
pub mod replace_lines;
