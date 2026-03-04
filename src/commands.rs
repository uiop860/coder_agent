pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: [SlashCommand; 5] = [
    SlashCommand {
        name: "/reasoning",
        description: "Toggle reasoning / thinking display",
    },
    SlashCommand {
        name: "/tools",
        description: "Toggle tool result messages",
    },
    SlashCommand {
        name: "/model",
        description: "Choose the OpenRouter model",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear the conversation context",
    },
    SlashCommand {
        name: "/exit",
        description: "Exit the application",
    },
];

pub struct ModelOption {
    pub label: &'static str,
    pub id: &'static str,
    /// Maximum context window in tokens for this model.
    pub context_window: u64,
}

pub const MODELS: [ModelOption; 4] = [
    ModelOption {
        label: "Nemotron 3 Nano 30B (free)",
        id: "nvidia/nemotron-3-nano-30b-a3b:free",
        context_window: 256_000,
    },
    ModelOption {
        label: "Trinity Large (free)",
        id: "arcee-ai/trinity-large-preview:free",
        context_window: 131_000,
    },
    ModelOption {
        label: "Step 3.5 Flash (free)",
        id: "stepfun/step-3.5-flash:free",
        context_window: 256_000,
    },
    ModelOption {
        label: "GLM-4.5 Air (free)",
        id: "z-ai/glm-4.5-air:free",
        context_window: 131_072,
    },
];

pub fn filtered_commands(input: &str) -> Vec<usize> {
    SLASH_COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, c)| c.name.starts_with(input))
        .map(|(i, _)| i)
        .collect()
}
