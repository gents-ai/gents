use crate::llm::tool::{Tool, ToolDefinition, ToolDyn};
use serde::Deserialize;
use serde_json::json;

use crate::goal::{CREATE_GOAL_TOOL_NAME, GET_GOAL_TOOL_NAME, UPDATE_GOAL_TOOL_NAME};

use super::shared::ToolError;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetGoalArgs {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateGoalArgs {
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGoalArgs {
    pub objective: String,
    #[serde(default)]
    pub token_budget: Option<i64>,
}

#[derive(Clone, Copy)]
struct GetGoalTool;

#[derive(Clone, Copy)]
struct UpdateGoalTool;

#[derive(Clone, Copy)]
struct CreateGoalTool;

impl Tool for CreateGoalTool {
    const NAME: &'static str = CREATE_GOAL_TOOL_NAME;
    type Error = ToolError;
    type Args = CreateGoalArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Create a durable goal owned by the current principal and session. Repeating the exact same objective and budget is idempotent; a different request conflicts. Ownership cannot be supplied by the model.".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "The concrete objective to pursue across continuation requests."
                    },
                    "token_budget": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": i64::MAX,
                        "description": "Optional positive aggregate token budget for the goal."
                    }
                },
                "required": ["objective"]
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Err(anyhow::anyhow!("create_goal is executed by the session persistence hook").into())
    }
}

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

pub fn build_goal_tools(include_creation: bool) -> Vec<Box<dyn ToolDyn>> {
    let mut tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(GetGoalTool), Box::new(UpdateGoalTool)];
    if include_creation {
        tools.push(Box::new(CreateGoalTool));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_arguments_reject_model_supplied_ownership() {
        assert!(serde_json::from_str::<CreateGoalArgs>(
            r#"{"objective":"ship","agent_did":"did:test:other"}"#,
        )
        .is_err());
        assert!(serde_json::from_str::<CreateGoalArgs>(
            r#"{"objective":"ship","session_id":"other-session"}"#,
        )
        .is_err());
        assert!(serde_json::from_str::<UpdateGoalArgs>(
            r#"{"status":"complete","session_id":"other-session"}"#,
        )
        .is_err());
    }

    #[test]
    fn goal_budget_deserialization_is_bounded_to_storage_type() {
        assert!(serde_json::from_str::<CreateGoalArgs>(
            r#"{"objective":"ship","token_budget":9223372036854775808}"#,
        )
        .is_err());
        let maximum = serde_json::from_str::<CreateGoalArgs>(
            r#"{"objective":"ship","token_budget":9223372036854775807}"#,
        )
        .unwrap();
        assert_eq!(maximum.token_budget, Some(i64::MAX));
    }
}
