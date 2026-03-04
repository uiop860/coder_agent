use std::sync::{
    Arc, Mutex,
    atomic::AtomicBool,
    mpsc::{Receiver, Sender as MpscSender},
};

use coder_agent::client::{AgentEvent, ChatMessage, Provider, RequestConfig, ToolCallInfo};

#[derive(Debug, Clone)]
pub enum Sender {
    User,
    Agent,
    /// Tool call / result messages — toggleable with Ctrl+D.
    Tool,
}

/// A rendered message in the TUI.  Most messages simply carry text and
/// optionally an in-progress reasoning buffer, but we also support a
/// special "tool call" message which stores the parsed information so it can
/// be shown or hidden on demand.
#[derive(Debug, Clone)]
pub struct Message {
    pub sender: Sender,
    pub content: String,
    pub reasoning: String,
    /// When this message represents a tool call, the parsed details are kept
    /// here so we can re-render it with or without the arguments as the user
    /// toggles `show_tool_call_details`.
    pub tool_call: Option<ToolCallInfo>,
    /// Preserved tool name for tool result messages (when tool_call is None but
    /// sender is Tool). Used to show minimal tool name when show_tools is false.
    pub tool_name: Option<String>,
    /// Pre-computed plain-text diff preview (with +/- prefixes) for
    /// replace_lines calls. Rendered with colour in the messages pane.
    pub diff_preview: Option<String>,
}

pub struct App {
    // Rendered message list
    pub messages: Vec<Message>,
    // Full conversation history sent to the API
    pub history: Vec<ChatMessage>,
    pub input_buffer: String,
    /// Byte index of the edit cursor within `input_buffer`.
    pub cursor_pos: usize,
    /// Vertical scroll offset for the input box (in lines). Auto-follows the cursor.
    pub input_scroll: u16,
    pub scroll_offset: usize,
    pub max_scroll: usize,
    pub scroll_up_held: bool,
    pub scroll_down_held: bool,
    // Live stream from the provider
    pub rx: Option<Receiver<AgentEvent>>,
    pub streaming: bool,
    pub provider: Option<Arc<dyn Provider>>,
    pub config: RequestConfig,
    /// When true we render the arguments payload attached to any tool-call
    /// messages.  Toggled by the user pressing Ctrl+T.
    pub show_tool_call_details: bool,
    /// When false, any reasoning tokens accumulated on messages are hidden
    /// from the view.  Toggled by Ctrl+R.
    pub show_reasoning: bool,
    /// When false we hide all `Tool`-sender messages.  Toggled by Ctrl+D.
    pub show_tools: bool,
    /// True when input starts with `/` and the slash-command popover is open.
    pub slash_mode: bool,
    /// Index of the highlighted row in the filtered slash-command list.
    pub slash_selected: usize,
    /// True when the model picker sub-menu is open (entered via /model).
    pub slash_model_mode: bool,
    /// Accumulated input tokens across all completed responses.
    pub total_input_tokens: u64,
    /// Accumulated output tokens across all completed responses.
    pub total_output_tokens: u64,
    /// Input token count from the most recent completed response (for context % display).
    pub last_input_tokens: u64,
    /// Animated display value for input tokens (scrolls toward total_input_tokens each frame).
    pub displayed_input_tokens: u64,
    /// Animated display value for output tokens (scrolls toward total_output_tokens each frame).
    pub displayed_output_tokens: u64,
    /// Frame counter used to drive the input-border pulse animation while streaming.
    pub pulse_tick: u64,
    /// Set to true to cancel the current agent stream.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Channel to send approval decisions (true = approve, false = deny) to the agent thread.
    pub approval_tx: Option<MpscSender<bool>>,
    /// Pending tool approval request waiting for user input.
    pub approval_pending: Option<ToolCallInfo>,
    /// Shared sender slot: set before each stream so InvokeSubagentTool can
    /// forward sub-agent ToolCall/ToolCallResult events to the TUI in real time.
    pub subagent_progress_tx: Arc<Mutex<Option<MpscSender<AgentEvent>>>>,
    /// Receiver for the sub-agent progress side-channel (drained each frame).
    pub subagent_rx: Option<Receiver<AgentEvent>>,
}
