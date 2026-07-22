//! Workflow-orchestration tool surface (#378).
//!
//! Cut 1 ships only `fan_out_and_synthesize`. The other three primitives
//! (`pipeline`, `verify`, `loop_until_done`) are intentionally NOT stubbed as a
//! common `OrchestrationPrimitive` trait yet: a trait with a single
//! implementation adds an abstraction (and dead `#[allow(dead_code)]` stub
//! types) with no consumer, which the codebase avoids. The real shared seam is
//! [`build_orchestration_tools`]/[`orchestration_tool_names`] (and the
//! `OrchestrationToolConfig` gate); cuts 2–4 introduce the trait when there is a
//! second implementation to factor against. This deviation from plan Task 1.4
//! Step 2 / design D8 is deliberate and recorded in the plan's self-review.

use anyhow::anyhow;

use crate::llm::tool::{Tool, ToolDefinition};
use crate::tool_surface::{OrchestrationToolConfig, SubagentToolConfig};
use crate::workflow::{
    FanOutAndSynthesizeArgs, FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, MAX_FAN_OUT_TASKS,
};

use super::shared::ToolError;

const ORCHESTRATION_SERVICE_ID: &str = "workflow";

#[derive(Clone)]
pub(super) struct FanOutAndSynthesizeTool {
    subagents: SubagentToolConfig,
}

impl FanOutAndSynthesizeTool {
    pub(super) fn new(subagents: SubagentToolConfig) -> Self {
        Self { subagents }
    }

    fn validate(&self, args: &FanOutAndSynthesizeArgs) -> Result<(), ToolError> {
        let width = args.fan_out_width();
        if width == 0 {
            return Err(invalid_arguments_error(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/tasks",
                "tasks must contain at least one fan-out task",
            ));
        }
        if width > MAX_FAN_OUT_TASKS {
            return Err(invalid_arguments_error(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/tasks",
                format!("tasks may contain at most {MAX_FAN_OUT_TASKS} fan-out tasks"),
            ));
        }

        let default_target = args
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty());
        if default_target.is_none() && args.tasks.iter().any(|task| task.target.is_none()) {
            return Err(invalid_arguments_error(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/target",
                "target is required unless every task carries its own target",
            ));
        }
        if let Some(target) = default_target {
            self.validate_target("/target", target)?;
        }
        for (index, task) in args.tasks.iter().enumerate() {
            if task.prompt.trim().is_empty() {
                return Err(invalid_arguments_error(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    format!("/tasks/{index}/prompt"),
                    "task prompt is required",
                ));
            }
            if let Some(target) = task
                .target
                .as_deref()
                .map(str::trim)
                .filter(|target| !target.is_empty())
            {
                self.validate_target(format!("/tasks/{index}/target"), target)?;
            }
        }

        let synthesis_target = args
            .synthesis_target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| {
                invalid_arguments_error(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/synthesis_target",
                    "synthesis_target is required",
                )
            })?;
        self.validate_target("/synthesis_target", synthesis_target)?;
        if args.synthesis_prompt.trim().is_empty() {
            return Err(invalid_arguments_error(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/synthesis_prompt",
                "synthesis_prompt is required",
            ));
        }
        Ok(())
    }

    fn validate_target(&self, path: impl Into<String>, target: &str) -> Result<(), ToolError> {
        if self
            .subagents
            .targets
            .iter()
            .any(|allowed| allowed.name == target)
        {
            return Ok(());
        }
        Err(tool_not_allowed_error(
            FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
            path,
            target,
            format!("'{target}' is not an allowed subagent target for this behavior"),
            self.allowed_target_names(),
        ))
    }

    fn allowed_target_names(&self) -> Vec<String> {
        self.subagents
            .targets
            .iter()
            .map(|target| target.name.clone())
            .collect()
    }
}

impl Tool for FanOutAndSynthesizeTool {
    const NAME: &'static str = FAN_OUT_AND_SYNTHESIZE_TOOL_NAME;

    type Error = ToolError;
    type Args = FanOutAndSynthesizeArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let allowed_names = self.allowed_target_names();
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Spawn one to eight authorized subagents, wait until every fan-out child is terminal, then run one synthesis subagent over the structured outcomes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "enum": allowed_names,
                        "description": "Default subagent target name for fan-out tasks."
                    },
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_FAN_OUT_TASKS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Optional stable task id used in the synthesis payload."
                                },
                                "target": {
                                    "type": "string",
                                    "enum": self.allowed_target_names(),
                                    "description": "Optional target override for this task."
                                },
                                "prompt": {
                                    "type": "string",
                                    "description": "Prompt to send to the fan-out child."
                                }
                            },
                            "required": ["prompt"]
                        }
                    },
                    "synthesis_target": {
                        "type": "string",
                        "enum": self.allowed_target_names(),
                        "description": "Subagent target that synthesizes the structured fan-out outcomes."
                    },
                    "synthesis_prompt": {
                        "type": "string",
                        "description": "Instruction for the synthesis child. The runtime appends the fan-out outcomes."
                    }
                },
                "required": ["target", "tasks", "synthesis_target", "synthesis_prompt"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.validate(&args)?;
        Err(not_yet_executable_error(Self::NAME))
    }
}

pub(crate) fn orchestration_tool_names(
    config: &OrchestrationToolConfig,
    subagents: &SubagentToolConfig,
) -> Vec<String> {
    if !orchestration_tools_enabled(config, subagents) {
        return Vec::new();
    }
    vec![FAN_OUT_AND_SYNTHESIZE_TOOL_NAME.to_string()]
}

pub(crate) fn build_orchestration_tools(
    config: OrchestrationToolConfig,
    subagents: SubagentToolConfig,
) -> Vec<Box<dyn crate::llm::tool::ToolDyn>> {
    if !orchestration_tools_enabled(&config, &subagents) {
        return Vec::new();
    }
    vec![Box::new(FanOutAndSynthesizeTool::new(subagents))]
}

fn orchestration_tools_enabled(
    config: &OrchestrationToolConfig,
    subagents: &SubagentToolConfig,
) -> bool {
    config.enabled
        && subagents.spawn_enabled
        && subagents.background_enabled
        && !subagents.targets.is_empty()
}

fn invalid_arguments_error(
    tool_name: &str,
    path: impl Into<String>,
    message: impl Into<String>,
) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "path": path.into(),
        "message": message.into(),
        "retryable": false,
        "service_id": ORCHESTRATION_SERVICE_ID,
        "tool_name": tool_name
    }))
}

fn tool_not_allowed_error(
    tool_name: &str,
    path: impl Into<String>,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: Vec<String>,
) -> ToolError {
    structured_error(serde_json::json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path.into(),
        "message": message.into(),
        "retryable": false,
        "service_id": ORCHESTRATION_SERVICE_ID,
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
        "message": format!("{tool_name} is hook-managed and cannot be executed outside the Gents session hook"),
        "retryable": true,
        "service_id": ORCHESTRATION_SERVICE_ID,
        "tool_name": tool_name
    }))
}

fn structured_error(error: serde_json::Value) -> ToolError {
    let message = serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string());
    anyhow!(message).into()
}
