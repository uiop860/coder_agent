use crate::tools::{Tool, ToolDefinition};
use serde_json::json;
use std::process::Command;

pub struct RunCommandTool;

impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command",
            description: "Run a shell command and return its output (stdout + stderr). \
                Use cwd to set the working directory. Output is truncated at 4000 characters.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the command (optional, defaults to .)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let command = args
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| "Missing or invalid 'command' parameter".to_string())?;

        let cwd = args.get("cwd").and_then(|c| c.as_str()).unwrap_or(".");

        #[cfg(target_os = "windows")]
        let output = Command::new("cmd")
            .args(["/C", command])
            .current_dir(cwd)
            .output();

        #[cfg(not(target_os = "windows"))]
        let output = Command::new("sh")
            .args(["-c", command])
            .current_dir(cwd)
            .output();

        let output = output.map_err(|e| format!("Failed to run command: {}", e))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut combined = String::new();
        combined.push_str(&stdout);
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        // Truncate at char boundary ≤ 4000 chars
        const MAX_CHARS: usize = 4000;
        let truncated = if combined.len() > MAX_CHARS {
            let end = combined
                .char_indices()
                .take_while(|(i, _)| *i < MAX_CHARS)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(MAX_CHARS);
            format!("{}...[truncated]", &combined[..end])
        } else {
            combined
        };

        Ok(format!("[exit {}]\n{}", exit_code, truncated))
    }
}
