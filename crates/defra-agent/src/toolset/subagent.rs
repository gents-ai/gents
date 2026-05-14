use anyhow::anyhow;
use rig::completion::ToolDefinition;
use rig::tool::Tool;

use crate::background_tools::r4c_args::{
    ListBackgroundToolsArgs, ListSubagentsArgs, ReadSubagentTranscriptArgs, ReadToolOutputArgs,
};
use crate::background_tools::{
    BackgroundToolArgs, CancelSubagentArgs, CancelToolArgs, SpawnSubagentArgs, WaitSubagentArgs,
    WaitToolArgs,
};
use crate::tool_call_lifecycle::AwaitMode;
use crate::tool_surface::{BackgroundToolConfig, SubagentToolConfig};

use super::shared::ToolError;
use super::{
    BACKGROUND_TOOL_NAME, CANCEL_SUBAGENT_TOOL_NAME, CANCEL_TOOL_NAME,
    LIST_BACKGROUND_TOOLS_TOOL_NAME, LIST_SUBAGENTS_TOOL_NAME, READ_SUBAGENT_TRANSCRIPT_TOOL_NAME,
    READ_TOOL_OUTPUT_TOOL_NAME, SPAWN_SUBAGENT_TOOL_NAME, WAIT_SUBAGENT_TOOL_NAME, WAIT_TOOL_NAME,
};

const SUBAGENT_SERVICE_ID: &str = "subagent";

#[derive(Clone)]
pub(super) struct SpawnSubagentTool {
    config: SubagentToolConfig,
}

#[derive(Clone)]
pub(super) struct BackgroundTool {
    config: BackgroundToolConfig,
}

impl SpawnSubagentTool {
    pub(super) fn new(config: SubagentToolConfig) -> Self {
        Self { config }
    }

    fn validate(&self, args: &SpawnSubagentArgs) -> Result<(), ToolError> {
        let behavior_id = args.behavior_id.trim();
        if behavior_id.is_empty() {
            return Err(invalid_arguments_error(
                SPAWN_SUBAGENT_TOOL_NAME,
                "/behavior_id",
                "behavior_id is required",
            ));
        }
        if !self
            .config
            .targets
            .iter()
            .any(|target| target == behavior_id)
        {
            return Err(tool_not_allowed_error(
                SPAWN_SUBAGENT_TOOL_NAME,
                "/behavior_id",
                behavior_id,
                format!(
                    "behavior '{behavior_id}' is not allowed as a subagent target for this behavior"
                ),
                self.config.targets.clone(),
            ));
        }
        if args.prompt.trim().is_empty() {
            return Err(invalid_arguments_error(
                SPAWN_SUBAGENT_TOOL_NAME,
                "/prompt",
                "prompt is required",
            ));
        }

        match args.await_mode.as_await_mode() {
            AwaitMode::Foreground => {}
            AwaitMode::Background if self.config.background_enabled => {}
            AwaitMode::Background => {
                return Err(tool_not_allowed_error(
                    SPAWN_SUBAGENT_TOOL_NAME,
                    "/await_mode",
                    "background",
                    "background subagent spawning is not enabled for this behavior",
                    self.config.targets.clone(),
                ));
            }
        }

        Ok(())
    }
}

impl BackgroundTool {
    pub(super) fn new(config: BackgroundToolConfig) -> Self {
        Self { config }
    }

    fn validate(&self, args: &BackgroundToolArgs) -> Result<(), ToolError> {
        let tool_name = args.tool_name.trim();
        if tool_name.is_empty() {
            return Err(background_invalid_arguments_error(
                BACKGROUND_TOOL_NAME,
                "/tool_name",
                "tool_name is required",
            ));
        }
        if !self.config.allowlist.iter().any(|name| name == tool_name) {
            return Err(background_tool_not_allowed_error(
                BACKGROUND_TOOL_NAME,
                "/tool_name",
                tool_name,
                format!("tool '{tool_name}' is not allowed for backgrounding by this behavior"),
                self.config.allowlist.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct WaitSubagentTool;

#[derive(Clone, Copy)]
pub(super) struct ListSubagentsTool;

#[derive(Clone, Copy)]
pub(super) struct ReadSubagentTranscriptTool;

#[derive(Clone, Copy)]
pub(super) struct CancelSubagentTool;

#[derive(Clone, Copy)]
pub(super) struct WaitTool;

#[derive(Clone, Copy)]
pub(super) struct ListBackgroundToolsTool;

#[derive(Clone, Copy)]
pub(super) struct ReadToolOutputTool;

#[derive(Clone, Copy)]
pub(super) struct CancelTool;

impl Tool for SpawnSubagentTool {
    const NAME: &'static str = SPAWN_SUBAGENT_TOOL_NAME;

    type Error = ToolError;
    type Args = SpawnSubagentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let await_modes = if self.config.background_enabled {
            vec!["foreground", "background"]
        } else {
            vec!["foreground"]
        };

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Spawn an authorized behavior as a child subagent. Foreground is the default await mode; background is available only when enabled for this behavior."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "behavior_id": {
                        "type": "string",
                        "enum": &self.config.targets,
                        "description": "Target behavior ID from this behavior's allowed subagent target set."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Task prompt to send to the child subagent."
                    },
                    "await_mode": {
                        "type": "string",
                        "enum": await_modes,
                        "default": "foreground",
                        "description": "Use foreground to wait for the child result. Use background only when the schema exposes it."
                    },
                    "deadline": {
                        "type": "string",
                        "format": "date-time",
                        "description": "Optional RFC3339 deadline for the child, bounded by the parent request deadline."
                    }
                },
                "required": ["behavior_id", "prompt"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.validate(&args)?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for WaitSubagentTool {
    const NAME: &'static str = WAIT_SUBAGENT_TOOL_NAME;

    type Error = ToolError;
    type Args = WaitSubagentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Wait for an existing child subagent request to reach a terminal state."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "child_request_id": {
                        "type": "string",
                        "description": "Child AgentRequest ID returned by spawn_subagent."
                    }
                },
                "required": ["child_request_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_child_request_id(WAIT_SUBAGENT_TOOL_NAME, &args.child_request_id)?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for CancelSubagentTool {
    const NAME: &'static str = CANCEL_SUBAGENT_TOOL_NAME;

    type Error = ToolError;
    type Args = CancelSubagentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Cancel an existing child subagent request and its live descendants."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "child_request_id": {
                        "type": "string",
                        "description": "Child AgentRequest ID returned by spawn_subagent."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional human-readable cancellation reason."
                    }
                },
                "required": ["child_request_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_child_request_id(CANCEL_SUBAGENT_TOOL_NAME, &args.child_request_id)?;
        if args
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(invalid_arguments_error(
                CANCEL_SUBAGENT_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            ));
        }
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for ListSubagentsTool {
    const NAME: &'static str = LIST_SUBAGENTS_TOOL_NAME;

    type Error = ToolError;
    type Args = ListSubagentsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List this parent request's visible background child subagents."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["running", "terminal", "all"],
                        "default": "running",
                        "description": "Filter child subagents by bridge lifecycle state."
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
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for ReadSubagentTranscriptTool {
    const NAME: &'static str = READ_SUBAGENT_TRANSCRIPT_TOOL_NAME;

    type Error = ToolError;
    type Args = ReadSubagentTranscriptArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read a compact transcript snapshot from a visible background subagent."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "child_request_id": {
                        "type": "string",
                        "description": "Child request ID returned by spawn_subagent or list_subagents."
                    },
                    "since_sequence": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0,
                        "description": "First transcript sequence to include."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "default": 20,
                        "description": "Maximum rendered message blocks to return."
                    },
                    "max_chars": {
                        "type": "integer",
                        "minimum": 64,
                        "maximum": 24000,
                        "default": 6000,
                        "description": "Maximum rendered transcript characters to return."
                    },
                    "include_user_messages": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include ordinary user messages from the child session."
                    },
                    "include_tool_results": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include capped tool-result snippets from the child session."
                    }
                },
                "required": ["child_request_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_child_request_id(Self::NAME, &args.child_request_id)?;
        let _ = args.validated_limit();
        let _ = args.validated_max_chars();
        Err(not_yet_executable_error(Self::NAME))
    }
}

impl Tool for BackgroundTool {
    const NAME: &'static str = BACKGROUND_TOOL_NAME;

    type Error = ToolError;
    type Args = BackgroundToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Run an allowlisted backgroundable tool and return a handle immediately."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "enum": &self.config.allowlist,
                        "description": "Allowlisted tool name to run in the background."
                    },
                    "args": {
                        "type": "object",
                        "description": "Arguments passed to the target tool."
                    }
                },
                "required": ["tool_name", "args"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.validate(&args)?;
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for WaitTool {
    const NAME: &'static str = WAIT_TOOL_NAME;

    type Error = ToolError;
    type Args = WaitToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Wait for an existing backgrounded tool call to reach a terminal state."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Tool call ID returned by background_tool."
                    }
                },
                "required": ["tool_call_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_tool_call_id(WAIT_TOOL_NAME, &args.tool_call_id)?;
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

impl Tool for ListBackgroundToolsTool {
    const NAME: &'static str = LIST_BACKGROUND_TOOLS_TOOL_NAME;

    type Error = ToolError;
    type Args = ListBackgroundToolsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List this parent request's visible background tool calls.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["running", "terminal", "all"],
                        "default": "running",
                        "description": "Filter background tool calls by lifecycle state."
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

impl Tool for ReadToolOutputTool {
    const NAME: &'static str = READ_TOOL_OUTPUT_TOOL_NAME;

    type Error = ToolError;
    type Args = ReadToolOutputArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read stdout and stderr snapshots for a visible background tool call."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Tool call ID returned by background_tool or list_background_tools."
                    },
                    "max_bytes_per_stream": {
                        "type": "integer",
                        "minimum": 256,
                        "maximum": 262144,
                        "default": 16384,
                        "description": "Maximum bytes to return per stream."
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

impl Tool for CancelTool {
    const NAME: &'static str = CANCEL_TOOL_NAME;

    type Error = ToolError;
    type Args = CancelToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Cancel an existing backgrounded tool call.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "Tool call ID returned by background_tool."
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
        validate_tool_call_id(CANCEL_TOOL_NAME, &args.tool_call_id)?;
        if args
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(background_invalid_arguments_error(
                CANCEL_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            ));
        }
        Err(background_not_yet_executable_error(Self::NAME))
    }
}

fn validate_child_request_id(tool_name: &str, child_request_id: &str) -> Result<(), ToolError> {
    if child_request_id.trim().is_empty() {
        return Err(invalid_arguments_error(
            tool_name,
            "/child_request_id",
            "child_request_id is required",
        ));
    }
    Ok(())
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
        "service_id": SUBAGENT_SERVICE_ID,
        "tool_name": tool_name
    }))
}

fn tool_not_allowed_error(
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
        "service_id": SUBAGENT_SERVICE_ID,
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_subagent_targets": allowed_targets
    }))
}

fn not_yet_executable_error(tool_name: &str) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "path": "/",
        "message": format!("{tool_name} is registered but requires the R4b subagent hook runtime path before direct execution"),
        "retryable": true,
        "service_id": SUBAGENT_SERVICE_ID,
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
        "service_id": "background",
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
        "service_id": "background",
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
        "message": format!("{tool_name} is registered but requires the R6 background-tool hook runtime path before direct execution"),
        "retryable": true,
        "service_id": "background",
        "tool_name": tool_name
    }))
}

fn structured_error(error: serde_json::Value) -> ToolError {
    let message = serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string());
    anyhow!(message).into()
}
