use anyhow::anyhow;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::subagent_tools::{SpawnSubagentArgs, WaitSubagentArgs};
use crate::tool_call_lifecycle::AwaitMode;
use crate::tool_surface::SubagentToolConfig;

use super::shared::ToolError;
use super::{CANCEL_SUBAGENT_TOOL_NAME, SPAWN_SUBAGENT_TOOL_NAME, WAIT_SUBAGENT_TOOL_NAME};

const SUBAGENT_SERVICE_ID: &str = "subagent";

#[derive(Clone)]
pub(super) struct SpawnSubagentTool {
    config: SubagentToolConfig,
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

#[derive(Clone, Copy)]
pub(super) struct WaitSubagentTool;

#[derive(Clone, Copy)]
pub(super) struct CancelSubagentTool;

#[derive(Debug, Deserialize)]
pub(super) struct CancelSubagentArgs {
    child_request_id: String,
    #[serde(default)]
    reason: Option<String>,
}

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

fn structured_error(error: serde_json::Value) -> ToolError {
    let message = serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string());
    anyhow!(message).into()
}
