pub const DEFRA_QUERY_TOOL_NAME: &str = "query";
pub const SCHEMA_TOOL_NAME: &str = "schema";
pub const CONFIG_TOOL_NAME: &str = "config";

pub const SELF_CONFIG_TOOL_NAMES: [&str; 1] = [CONFIG_TOOL_NAME];

pub const META_TOOL_NAMES: [&str; 3] = ["discover_tools", "describe_tool", "call_tool"];

pub const CONTEXT_BUDGET_TOOL_NAME: &str = "context_budget";
pub const SESSION_HISTORY_TOOL_NAME: &str = "sessions";
pub const LSP_TOOL_NAME: &str = "lsp";

pub const AGENT_NEW_TOOL_NAME: &str = "agent_new";
pub const AGENT_MESSAGE_TOOL_NAME: &str = "agent_message";
pub const AGENT_INTERRUPT_TOOL_NAME: &str = "agent_interrupt";
pub const AGENT_STOP_TOOL_NAME: &str = "agent_stop";
pub const AGENT_RESUME_TOOL_NAME: &str = "agent_resume";
pub const AGENT_LIST_TOOL_NAME: &str = "agent_list";
pub const AGENT_SESSION_LIST_TOOL_NAME: &str = "agent_session_list";
pub const AGENT_SESSION_READ_TOOL_NAME: &str = "agent_session_read";
pub const AGENT_TOOL_NAMES: [&str; 8] = [
    AGENT_NEW_TOOL_NAME,
    AGENT_MESSAGE_TOOL_NAME,
    AGENT_INTERRUPT_TOOL_NAME,
    AGENT_STOP_TOOL_NAME,
    AGENT_RESUME_TOOL_NAME,
    AGENT_LIST_TOOL_NAME,
    AGENT_SESSION_LIST_TOOL_NAME,
    AGENT_SESSION_READ_TOOL_NAME,
];
pub const SPAWN_PROCESS_TOOL_NAME: &str = "spawn_process";
pub const WAIT_PROCESS_TOOL_NAME: &str = "wait_process";
pub const LIST_PROCESSES_TOOL_NAME: &str = "list_processes";
pub const READ_PROCESS_TOOL_NAME: &str = "read_process";
pub const CANCEL_PROCESS_TOOL_NAME: &str = "cancel_process";

pub const LIST_FILES_TOOL_NAME: &str = "list_files";
pub const READ_FILE_TOOL_NAME: &str = "read_file";
pub const GLOB_TOOL_NAME: &str = "glob";
pub const GREP_TOOL_NAME: &str = "grep";
pub const WRITE_FILE_TOOL_NAME: &str = "write_file";
pub const EDIT_FILE_TOOL_NAME: &str = "edit_file";
pub const BASH_TOOL_NAME: &str = "bash";
pub const BASH_UNRESTRICTED_TOOL_NAME: &str = "bash_unrestricted";
pub const NATIVE_TOOL_NAMES: [&str; 8] = [
    LIST_FILES_TOOL_NAME,
    READ_FILE_TOOL_NAME,
    GLOB_TOOL_NAME,
    GREP_TOOL_NAME,
    WRITE_FILE_TOOL_NAME,
    EDIT_FILE_TOOL_NAME,
    BASH_TOOL_NAME,
    BASH_UNRESTRICTED_TOOL_NAME,
];

pub fn is_session_message_tool(tool_name: &str) -> bool {
    tool_name == AGENT_NEW_TOOL_NAME || tool_name == AGENT_MESSAGE_TOOL_NAME
}
