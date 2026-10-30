use crate::inference::llm::tool::{Tool, ToolDefinition, ToolDyn};
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
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub token_budget: Option<i64>,
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
            description: "Create a durable goal owned by the current node and session. Repeating the exact same objective and budget is idempotent; a different request conflicts. Ownership cannot be supplied by the model.".to_string(),
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
                        "description": "Optional positive aggregate token budget for the goal. Omit it to create an uncapped goal with no aggregate limit; there is no default cap."
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
            description: "Edit the active goal's objective or token budget, or mark it complete or blocked. Omit status when editing; edits apply only while the goal is active. Completion requires the objective to be achieved; blocked requires the same condition across at least three consecutive goal turns. When the goal is blocked, paused, or usage_limited, edits and reactivation are refused: an operator resumes it with `gents goal resume-request --session SESSION --from REQUEST_ID`. For usage_limited, the operator must also resolve the account limit before resuming. A complete goal is terminal and a budget_limited goal cannot be reactivated: neither has a resume path.".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["active", "complete", "blocked"]
                    },
                    "reason": {
                        "type": "string",
                        "description": "Concise completion evidence or the repeated blocking condition."
                    },
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Revised objective for the active goal. Omit to keep it."
                    },
                    "token_budget": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": i64::MAX,
                        "description": "Positive aggregate goal budget. Omit to keep the goal's existing budget unchanged (an omitted budget never clears or resets an explicit cap). Budget changes apply to the active goal only; a budget_limited goal cannot be reactivated here."
                    }
                }
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
            r#"{"objective":"ship","node_did":"did:test:other"}"#,
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
    fn goal_arguments_reject_model_supplied_auto_resume() {
        assert!(serde_json::from_str::<UpdateGoalArgs>(
            r#"{"status":"active","auto_resume_at_reset":true}"#,
        )
        .is_err());
        assert!(serde_json::from_str::<CreateGoalArgs>(
            r#"{"objective":"x","auto_resume_at_reset":true}"#,
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

    #[tokio::test]
    async fn create_goal_help_states_omitted_budget_is_uncapped_with_no_default_cap() {
        let tools = build_goal_tools(true);
        let create = tools
            .iter()
            .find(|tool| tool.name() == CREATE_GOAL_TOOL_NAME)
            .expect("create_goal is advertised when creation is enabled");
        let definition = create.definition(String::new()).await;
        let budget = &definition.parameters["properties"]["token_budget"];
        let description = budget["description"].as_str().expect("budget description");
        assert!(
            description.contains("uncapped") && description.contains("no default cap"),
            "create_goal budget help must say an omitted budget is uncapped with no default cap, got: {description}"
        );
        assert_eq!(
            budget["required"],
            serde_json::Value::Null,
            "token_budget must stay optional in the create_goal schema"
        );
        assert!(
            budget.get("default").is_none(),
            "create_goal must not advertise a default budget value on the token_budget property"
        );
    }

    #[tokio::test]
    async fn update_goal_help_states_omission_preserves_cap_and_no_reactivation() {
        let tools = build_goal_tools(false);
        let update = tools
            .iter()
            .find(|tool| tool.name() == UPDATE_GOAL_TOOL_NAME)
            .expect("update_goal is always advertised");
        let definition = update.definition(String::new()).await;
        assert!(definition
            .description
            .contains("edits apply only while the goal is active"));
        assert!(definition
            .description
            .contains("gents goal resume-request --session SESSION --from REQUEST_ID"));
        let description = definition.parameters["properties"]["token_budget"]["description"]
            .as_str()
            .expect("budget description");
        assert!(
            description.contains("Omit to keep the goal's existing budget unchanged"),
            "update_goal budget help must say an omitted budget preserves the existing cap, got: {description}"
        );
        assert!(
            description.contains("budget_limited goal cannot be reactivated"),
            "update_goal budget help must say a budget_limited goal cannot be reactivated, got: {description}"
        );
        assert_eq!(
            definition.parameters["properties"]["status"]["enum"],
            serde_json::json!(["active", "complete", "blocked"]),
            "update_goal must only offer active/complete/blocked; budget_limited is never a target"
        );
    }

    #[test]
    fn update_goal_cannot_resume_from_budget_limited() {
        use crate::goal::{GoalAction, GoalState, GoalStatus};

        // The state machine itself must have no Resume edge out of
        // budget_limited, independent of any help text.
        let budget_limited = GoalState {
            status: GoalStatus::BudgetLimited,
            blocked_audits: 0,
            wrapup_requested: true,
            wrapup_completed: false,
        };
        assert_eq!(
            budget_limited.step(GoalAction::Resume),
            None,
            "Resume from budget_limited must be an illegal transition"
        );
        // And the operator transition helper must reject it too.
        assert!(
            crate::goal::apply_operator_status_transition(budget_limited, GoalStatus::Active)
                .is_err(),
            "update_goal to active from budget_limited must be rejected"
        );
    }
}
