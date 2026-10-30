use crate::inference::llm::tool::Tool;
use crate::inference::llm::tool::ToolDefinition;
use anyhow::anyhow;

use crate::background_tools::r4c_args::{ListBackgroundToolsArgs, ReadToolOutputArgs};
use crate::background_tools::{
    BackgroundToolArgs, CancelToolArgs, WaitToolArgs, DEFAULT_WAIT_PROCESS_TIMEOUT_SECS,
    MAX_WAIT_PROCESS_TIMEOUT_SECS,
};
use crate::session::message::{
    AgentInterruptArgs, AgentListArgs, AgentMessageArgs, AgentNewArgs, AgentSessionListArgs,
    AgentSessionReadArgs,
};
use crate::tool_surface::{AgentToolConfig, BackgroundToolConfig};

use super::shared::ToolError;
use super::{
    AGENT_INTERRUPT_TOOL_NAME, AGENT_LIST_TOOL_NAME, AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME,
    AGENT_RESUME_TOOL_NAME, AGENT_SESSION_LIST_TOOL_NAME, AGENT_SESSION_READ_TOOL_NAME,
    AGENT_STOP_TOOL_NAME, CANCEL_PROCESS_TOOL_NAME, LIST_PROCESSES_TOOL_NAME,
    READ_PROCESS_TOOL_NAME, SPAWN_PROCESS_TOOL_NAME, WAIT_PROCESS_TOOL_NAME,
};

const SESSION_SERVICE_ID: &str = "session";

#[derive(Clone)]
pub(super) struct AgentNewTool {
    config: AgentToolConfig,
}

#[derive(Clone, Copy)]
pub(super) struct AgentMessageTool;

#[derive(Clone, Copy)]
pub(super) struct AgentInterruptTool;

#[derive(Clone, Copy)]
pub(super) struct AgentStopTool;

#[derive(Clone, Copy)]
pub(super) struct AgentResumeTool;

#[derive(Clone, Copy)]
pub(super) struct AgentListTool;

#[derive(Clone, Copy)]
pub(super) struct AgentSessionListTool;

#[derive(Clone, Copy)]
pub(super) struct AgentSessionReadTool;

#[derive(Clone)]
pub(super) struct SpawnProcessTool {
    config: BackgroundToolConfig,
}

impl AgentNewTool {
    pub(super) fn new(config: AgentToolConfig) -> Self {
        Self { config }
    }

    fn allowed_target_names(&self) -> Vec<String> {
        self.config
            .targets
            .iter()
            .map(|target| target.name.clone())
            .collect()
    }
}

impl SpawnProcessTool {
    pub(super) fn new(config: BackgroundToolConfig) -> Self {
        Self { config }
    }

    fn validate(&self, args: &BackgroundToolArgs) -> Result<(), ToolError> {
        let tool_name = args.tool_name.trim();
        if tool_name.is_empty() {
            return Err(background_invalid_arguments_error(
                SPAWN_PROCESS_TOOL_NAME,
                "/tool_name",
                "tool_name is required",
            ));
        }
        if !self.config.allowlist.iter().any(|name| name == tool_name) {
            return Err(background_tool_not_allowed_error(
                SPAWN_PROCESS_TOOL_NAME,
                "/tool_name",
                tool_name,
                format!("tool '{tool_name}' is not allowed for backgrounding by this agent"),
                self.config.allowlist.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct WaitProcessTool;

#[derive(Clone, Copy)]
pub(super) struct ListProcessesTool;

#[derive(Clone, Copy)]
pub(super) struct ReadProcessTool;

#[derive(Clone, Copy)]
pub(super) struct CancelProcessTool;

fn message_body_schema(field: &str) -> serde_json::Value {
    let mut schema = serde_json::json!({
        "task": {
            "type": "object",
            "additionalProperties": false,
            "required": ["task_id"],
            "properties": {
                "task_id": { "type": "string", "description": "Task whose prompt is rendered and whose Goal, if declared, is set on the session." },
                "input": { "type": "object", "description": "Arguments rendered into the Task prompt." }
            },
            "description": format!("Configured Task to render. Provide exactly one of {field} or task.")
        }
    });
    schema[field] = serde_json::json!({
        "type": "string",
        "description": format!("Text to send. Provide exactly one of {field} or task.")
    });
    schema
}

impl Tool for AgentNewTool {
    const NAME: &'static str = AGENT_NEW_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentNewArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = message_body_schema("prompt");
        properties["agent"] = serde_json::json!({
            "type": "string",
            "enum": self.allowed_target_names(),
            "description": agent_target_name_description(&self.config.targets)
        });
        properties["title"] = serde_json::json!({
            "type": "string",
            "description": "Optional title for the new session."
        });
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Starts an agent on a task in the background; its result arrives later as a message in this conversation. Returns session_id, request_id and tool_call_id immediately. End your turn instead of polling. Continue the session with agent_message, stop its current turn with agent_interrupt, and kill the call with cancel_process."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["agent"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let agent = args.agent.trim();
        if self.config.target(agent).is_none() {
            return Err(structured_error(serde_json::json!({
                "ok": false,
                "failure_class": "tool_not_allowed",
                "path": "/agent",
                "message": format!("'{agent}' is not an allowed agent for this agent"),
                "retryable": false,
                "service_id": SESSION_SERVICE_ID,
                "tool_name": Self::NAME,
                "allowed_agents": self.allowed_target_names()
            })));
        }
        args.body()
            .map_err(|message| invalid_arguments_error(Self::NAME, "/", message))?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentMessageTool {
    const NAME: &'static str = AGENT_MESSAGE_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentMessageArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = message_body_schema("message");
        properties["session_id"] = serde_json::json!({
            "type": "string",
            "description": "An agent session you can reach (see agent_session_list), never this conversation's own session."
        });
        properties["interrupt"] = serde_json::json!({
            "type": "boolean",
            "description": "Legacy interrupt flag; if delivery is also supplied, true requires interrupt and false requires queue or steer."
        });
        properties["delivery"] = serde_json::json!({
            "type": "string",
            "enum": ["queue", "steer", "interrupt"],
            "description": "Queue after the current turn (default), steer at the next tool boundary, or interrupt the current turn before delivery when authorized."
        });
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Send a message to an agent session you can reach. An idle session starts a new turn; choose queue, steer, or interrupt for a busy session. Returns request_id, delivery (request, queue or steer) and tool_call_id immediately; the result arrives later as a message in this conversation."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["session_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.session_id.trim().is_empty() {
            return Err(invalid_arguments_error(
                Self::NAME,
                "/session_id",
                "session_id is required",
            ));
        }
        args.body()
            .map_err(|message| invalid_arguments_error(Self::NAME, "/", message))?;
        args.delivery()
            .map_err(|message| invalid_arguments_error(Self::NAME, "/delivery", message))?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentInterruptTool {
    const NAME: &'static str = AGENT_INTERRUPT_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentInterruptArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Interrupt the current turn and pause its Goal, preserving ordinary queued messages. Allowed for sessions this conversation started and same-node sessions opened by a Task trigger. agent_message queues further input; agent_resume resumes paused Goal work."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "A session this conversation started with agent_new."
                    }
                },
                "required": ["session_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.session_id.trim().is_empty() {
            return Err(invalid_arguments_error(
                Self::NAME,
                "/session_id",
                "session_id is required",
            ));
        }
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentStopTool {
    const NAME: &'static str = AGENT_STOP_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentInterruptArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Stop a same-node session, drain its queued requests, and pause its Goal until agent_resume is called. Stop may return while owned work is still settling; inspect agent_session_read with view=status before assuming cancellation completed. Remote sessions are not supported.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"session_id": {"type": "string"}},
                "required": ["session_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.session_id.trim().is_empty() {
            return Err(invalid_arguments_error(
                Self::NAME,
                "/session_id",
                "session_id is required",
            ));
        }
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentResumeTool {
    const NAME: &'static str = AGENT_RESUME_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentInterruptArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Resume a same-node session previously paused by agent_interrupt or agent_stop. This explicitly clears the stop latch and resumes eligible Goal work. Remote sessions are not supported.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"session_id": {"type": "string"}},
                "required": ["session_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.session_id.trim().is_empty() {
            return Err(invalid_arguments_error(
                Self::NAME,
                "/session_id",
                "session_id is required",
            ));
        }
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentListTool {
    const NAME: &'static str = AGENT_LIST_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentListArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List the configured agents you can start with agent_new, with their names and descriptions. Find existing sessions with agent_session_list."
                .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentSessionListTool {
    const NAME: &'static str = AGENT_SESSION_LIST_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentSessionListArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Find existing sessions you may message, including trigger-started work. Returns activity, relationship and message/interrupt permissions. Continue with next_cursor and the same filter; a stale cursor restarts the listing.".to_owned(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "filter": {
                        "type": "object", "additionalProperties": false,
                        "properties": {
                            "agent": {"type": "string", "description": "Target name or agent ID."},
                            "status": {"type": "string", "enum": ["busy", "idle"]},
                            "relationship": {"type": "string", "enum": ["started_by_you", "started_you", "messaged", "messaged_you", "same_node", "allowed_target"]},
                            "caused_by_trigger_id": {"type": "string"},
                            "caused_by_correlation": {"type": "string"},
                            "created_after": {"type": "string", "format": "date-time"},
                            "updated_after": {"type": "string", "format": "date-time"}
                        }
                    },
                    "cursor": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20}
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        args.validate()
            .map_err(|error| invalid_arguments_error(Self::NAME, "/", error.to_string()))?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for AgentSessionReadTool {
    const NAME: &'static str = AGENT_SESSION_READ_TOOL_NAME;

    type Error = ToolError;
    type Args = AgentSessionReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Inspect a session you may message. last returns its latest assistant message and request state; recent pages backward through bounded transcript text and tool summaries; status shows the active request or, if none, the latest one: work in flight, queued steering and the Goal. Truncation is reported; tool arguments and reasoning are omitted.".to_owned(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "required": ["session_id"],
                "properties": {
                    "session_id": {"type": "string", "description": "Discover available sessions with agent_session_list."},
                    "view": {"type": "string", "enum": ["last", "recent", "status"], "default": "last"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 6, "default": 6},
                    "cursor": {"type": "string", "description": "next_cursor from a recent page for this session."}
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        args.validate()
            .map_err(|error| invalid_arguments_error(Self::NAME, "/", error.to_string()))?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for SpawnProcessTool {
    const NAME: &'static str = SPAWN_PROCESS_TOOL_NAME;

    type Error = ToolError;
    type Args = BackgroundToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Spawn a background process by running an allowlisted long-running tool (e.g. a shell command). Returns a process handle immediately. You are notified automatically when it completes — end your turn instead of polling or sleep-waiting; use wait_process only for short bounded waits."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "enum": &self.config.allowlist,
                        "description": "Allowlisted tool name to run as a background process."
                    },
                    "args": {
                        "type": "object",
                        "description": "Arguments passed to the target tool."
                    }
                },
                "required": ["tool_name", "args"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.validate(&args)?;
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for WaitProcessTool {
    const NAME: &'static str = WAIT_PROCESS_TOOL_NAME;

    type Error = ToolError;
    type Args = WaitToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Wait up to timeout_secs for a background process in this session to reach a terminal state, including a handle returned on an earlier turn. A wait timeout, caller interruption, or caller deadline returns status \"running\" without cancelling the process. You are notified when it completes, so prefer ending your turn over waiting repeatedly. Default {DEFAULT_WAIT_PROCESS_TIMEOUT_SECS}s unless configured for that process; never more than {MAX_WAIT_PROCESS_TIMEOUT_SECS}s, or less if configured."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Process handle returned by spawn_process."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WAIT_PROCESS_TIMEOUT_SECS,
                        "description": "How long to wait before returning a still-running snapshot; the process is never cancelled by a wait timeout."
                    }
                },
                "required": ["tool_call_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_tool_call_id(WAIT_PROCESS_TOOL_NAME, &args.tool_call_id)?;
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for ListProcessesTool {
    const NAME: &'static str = LIST_PROCESSES_TOOL_NAME;

    type Error = ToolError;
    type Args = ListBackgroundToolsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List background processes manageable by the current requester in this session, including processes started on earlier turns.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["running", "terminal", "all"],
                        "default": "running",
                        "description": "Filter background processes by lifecycle state."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 20,
                        "description": "Maximum entries to return."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let _ = args.validated_limit();
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for ReadProcessTool {
    const NAME: &'static str = READ_PROCESS_TOOL_NAME;

    type Error = ToolError;
    type Args = ReadToolOutputArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read an incremental, byte-addressed slice of a manageable background process in this session, including one started on an earlier turn. \
captured output. stdout and stderr are merged in capture order behind a single byte cursor \
(stdout first, then a `--- stderr ---` boundary, then stderr), so you page through ALL output \
gap-free with one `offset`. The response returns `output` (the slice), `next_offset` (= offset + \
bytes returned; the exact cursor to pass next), `total_bytes` (total captured so far), `has_more` \
(true when `next_offset < total_bytes`), and `exited`/`exit_code` (whether the process finished). \
To read everything, start at offset 0 and loop with `offset = next_offset` until `has_more` is \
false. Pages are contiguous from the cursor; nothing in the middle is dropped."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Process handle returned by spawn_process or list_processes."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0,
                        "description": "Byte cursor into the combined output (0 = from start). Reads forward from here. Pass the prior response's `next_offset` to continue gap-free."
                    },
                    "max_tokens": {
                        "type": "integer",
                        "minimum": 64,
                        "maximum": 65536,
                        "default": 4096,
                        "description": "Token budget for the returned slice (estimated as chars/4). When the budget caps the slice, `has_more` is true and `next_offset` marks the resume point."
                    }
                },
                "required": ["tool_call_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_tool_call_id(Self::NAME, &args.tool_call_id)?;
        let _ = args.validated_max_bytes();
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for CancelProcessTool {
    const NAME: &'static str = CANCEL_PROCESS_TOOL_NAME;

    type Error = ToolError;
    type Args = CancelToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Cancel a manageable running background process in this session, including one started on an earlier turn.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Process handle returned by spawn_process."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional human-readable cancellation reason."
                    }
                },
                "required": ["tool_call_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_tool_call_id(CANCEL_PROCESS_TOOL_NAME, &args.tool_call_id)?;
        if args
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(background_invalid_arguments_error(
                CANCEL_PROCESS_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            ));
        }
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

fn agent_target_name_description(targets: &[crate::documents::AgentTargetDocument]) -> String {
    let mut description = String::from(
        "Friendly name of the agent to start a session with, from this agent's allowed targets.",
    );
    let entries: Vec<String> = targets
        .iter()
        .map(|target| {
            let desc = target.description.as_deref().unwrap_or_default().trim();
            if desc.is_empty() {
                format!("'{}'", target.name)
            } else {
                format!("'{}': {}", target.name, desc)
            }
        })
        .collect();
    if !entries.is_empty() {
        description.push_str(" Available: ");
        description.push_str(&entries.join("; "));
        description.push('.');
    }
    description
}

fn validate_tool_call_id(tool_name: &str, tool_call_id: &str) -> Result<(), ToolError> {
    if tool_call_id.trim().is_empty() {
        return Err(background_invalid_arguments_error(
            tool_name,
            "/tool_call_id",
            "tool_call_id is required",
        ));
    }
    Ok(())
}

fn invalid_arguments_error(tool_name: &str, path: &str, message: impl Into<String>) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": SESSION_SERVICE_ID,
        "tool_name": tool_name
    }))
}

fn not_yet_executable_error(tool_name: &str) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "path": "/",
        "message": format!("{tool_name} is registered but runs only through the session hook"),
        "retryable": true,
        "service_id": SESSION_SERVICE_ID,
        "tool_name": tool_name
    }))
}

fn background_invalid_arguments_error(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "process",
        "tool_name": tool_name
    }))
}

fn background_tool_not_allowed_error(
    tool_name: &str,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: Vec<String>,
) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "process",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_backgroundable_tool_names": allowed_targets
    }))
}

fn background_not_yet_executable_error(tool_name: &str) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "path": "/",
        "message": format!("{tool_name} is registered but requires the R6 process hook runtime path before direct execution"),
        "retryable": true,
        "service_id": "process",
        "tool_name": tool_name
    }))
}

fn structured_error(error: serde_json::Value) -> ToolError {
    let message = crate::tool_output::render(
        &error,
        &[
            "message",
            "allowed_agents",
            "ok",
            "retryable",
            "failure_class",
            "path",
        ],
    )
    .unwrap_or_else(|_| error.to_string());
    anyhow!(message).into()
}
