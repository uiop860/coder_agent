use crate::tools::{Tool, ToolDefinition};
use regex::Regex;
use serde_json::json;
use std::fs;
use std::path::Path;

pub struct GrepCodeTool;

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];
const MAX_MATCHES: usize = 100;

fn is_binary(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => {
            let check_len = bytes.len().min(512);
            bytes[..check_len].contains(&0u8)
        }
        Err(_) => true,
    }
}

fn glob_matches(filename: &str, glob: &str) -> bool {
    if let Some(ext) = glob.strip_prefix("*.") {
        filename.ends_with(&format!(".{}", ext))
    } else {
        filename == glob
    }
}

fn search_dir(dir: &Path, re: &Regex, glob: Option<&str>, results: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            search_dir(&path, re, glob, results);
        } else if path.is_file() {
            if let Some(g) = glob
                && !glob_matches(&name, g)
            {
                continue;
            }
            if is_binary(&path) {
                continue;
            }
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (line_num, line) in content.lines().enumerate() {
                if results.len() >= MAX_MATCHES {
                    return;
                }
                if re.is_match(line) {
                    results.push(format!("{}:{}: {}", path.display(), line_num + 1, line));
                }
            }
        }
    }
}

impl Tool for GrepCodeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep_code",
            description: "Search file contents using a regex pattern. \
                Skips .git, target, and node_modules directories. \
                Returns up to 100 matches in 'path:line: content' format.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "root": {
                        "type": "string",
                        "description": "Root directory to search from (optional, defaults to .)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional file glob filter, e.g. *.rs or *.ts"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse arguments: {}", e))?;

        let pattern = args
            .get("pattern")
            .and_then(|p| p.as_str())
            .ok_or_else(|| "Missing or invalid 'pattern' parameter".to_string())?;

        let root = args.get("root").and_then(|r| r.as_str()).unwrap_or(".");
        let glob = args.get("glob").and_then(|g| g.as_str());

        let re = Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;

        let root_path = Path::new(root);
        if !root_path.exists() {
            return Err(format!("Root path '{}' does not exist", root));
        }

        let mut results: Vec<String> = Vec::new();
        search_dir(root_path, &re, glob, &mut results);

        if results.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            let truncated = if results.len() == MAX_MATCHES {
                format!(
                    "{}\n[truncated at {} matches]",
                    results.join("\n"),
                    MAX_MATCHES
                )
            } else {
                results.join("\n")
            };
            Ok(truncated)
        }
    }
}
