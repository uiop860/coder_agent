use std::sync::Arc;

use crate::tools::{self, Tool};

/// Describes a named sub-agent configuration.
pub struct AgentProfile {
    pub name: &'static str,
    pub description: &'static str,
    pub system_prompt: &'static str,
    /// Function pointer that constructs the tool set for this profile.
    pub make_tools: fn() -> Vec<Arc<dyn Tool>>,
}

pub const AGENT_PROFILES: &[AgentProfile] = &[AgentProfile {
    name: "Reader",
    description: "Read-only agent that can explore the codebase but cannot modify files.",
    system_prompt: "You can read and explore the codebase but cannot modify files. \
Use the available tools to inspect files and directories, then provide a clear summary.",
    make_tools: readonly_tools,
}];

pub fn get_profile(name: &str) -> Option<&'static AgentProfile> {
    AGENT_PROFILES.iter().find(|p| p.name == name)
}

fn readonly_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(tools::read_file::ReadFileTool),
        Arc::new(tools::list_dir::ListDirTool),
        Arc::new(tools::search_file::SearchFileTool),
        Arc::new(tools::current_dir::CurrentDirTool),
        Arc::new(tools::grep_code::GrepCodeTool),
    ]
}
