use crate::llm::tool::{Tool, ToolDefinition, ToolDyn};
use serde::Deserialize;
use serde_json::json;

use crate::goal::{GET_GOAL_TOOL_NAME, UPDATE_GOAL_TOOL_NAME};

use super::shared::ToolError;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GetGoalArgs {}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGoalArgs {
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Copy)]
struct GetGoalTool;

#[derive(Clone, Copy)]
struct UpdateGoalTool;

impl Tool for GetGoalTool {
    const NAME: &'static str = GET_GOAL_TOOL_NAME;
    type Error = ToolError;
    type Args = GetGoalArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read the durable goal attached to the current session, including status, charged token usage, active time, and continuation audit state.".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Err(anyhow::anyhow!("get_goal is executed by the session persistence hook").into())
    }
}

impl Tool for UpdateGoalTool {
    const NAME: &'static str = UPDATE_GOAL_TOOL_NAME;
    type Error = ToolError;
    type Args = UpdateGoalArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Update the current durable goal only when it is genuinely complete or durably blocked. Blocked requires the same condition across at least three consecutive goal turns; the runtime enforces the threshold.".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["complete", "blocked"]
                    },
                    "reason": {
                        "type": "string",
                        "description": "Concise completion evidence or the repeated blocking condition."
                    }
                },
                "required": ["status"]
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Err(anyhow::anyhow!("update_goal is executed by the session persistence hook").into())
    }
}

pub fn build_goal_tools() -> Vec<Box<dyn ToolDyn>> {
    vec![Box::new(GetGoalTool), Box::new(UpdateGoalTool)]
}
