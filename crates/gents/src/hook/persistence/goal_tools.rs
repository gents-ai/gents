use chrono::Utc;
use serde_json::json;

use crate::goal::{
    create_goal_for_session, load_canonical_goal, next_blocked_audit, refresh_goal_usage,
    update_goal_fields, CreateGoalForSessionError, GoalAction, GoalSnapshot, GoalStatus,
    BLOCKED_AUDIT_THRESHOLD, CREATE_GOAL_TOOL_NAME, GET_GOAL_TOOL_NAME, UPDATE_GOAL_TOOL_NAME,
};
use crate::graphql::escape_graphql_string;
use crate::llm::ToolCallHookAction;
use crate::tool_call_lifecycle::ToolCallLifecycle;
use crate::toolset::{CreateGoalArgs, GetGoalArgs, UpdateGoalArgs};

use super::DefraSessionHook;

impl DefraSessionHook {
    pub(super) async fn persist_create_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, sequence) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );
        let mut lifecycle = ToolCallLifecycle::new(
            self.node.clone(),
            request_id,
            session_id.clone(),
            self.agent_did.clone(),
            internal_call_id.to_string(),
            sequence,
            CREATE_GOAL_TOOL_NAME.to_string(),
            args.to_string(),
            deadline_at,
        )
        .with_requester_did(self.active_requester_did().await)
        .with_request_doc_id(self.active_request_doc_id().await);
        lifecycle.start_running().await?;
        let parsed = match serde_json::from_str::<CreateGoalArgs>(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                let result = json!({
                    "accepted": false,
                    "disposition": "invalid",
                    "error": format!("invalid create_goal arguments: {error}"),
                })
                .to_string();
                lifecycle.complete(&result).await?;
                return Ok(self.skip_tool_result(CREATE_GOAL_TOOL_NAME, result));
            }
        };
        let outcome = match create_goal_for_session(
            &self.node,
            &self.agent_did,
            &session_id,
            &parsed.objective,
            parsed.token_budget,
        )
        .await
        {
            Ok(outcome) => json!({
                "accepted": true,
                "disposition": outcome.disposition(),
                "goal": GoalSnapshot::from_document(outcome.goal(), Utc::now()),
            }),
            Err(
                error @ (CreateGoalForSessionError::InvalidObjective
                | CreateGoalForSessionError::InvalidBudget),
            ) => json!({
                "accepted": false,
                "disposition": "invalid",
                "error": error.to_string(),
            }),
            Err(error @ CreateGoalForSessionError::Conflict) => json!({
                "accepted": false,
                "disposition": "conflict",
                "error": error.to_string(),
            }),
            Err(error @ CreateGoalForSessionError::Storage(_)) => json!({
                "accepted": false,
                "disposition": "error",
                "error": error.to_string(),
            }),
        };
        let result = serde_json::to_string_pretty(&outcome)?;
        lifecycle.complete(&result).await?;
        Ok(self.skip_tool_result(CREATE_GOAL_TOOL_NAME, result))
    }

    pub(super) async fn persist_get_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, sequence) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );
        let mut lifecycle = ToolCallLifecycle::new(
            self.node.clone(),
            request_id,
            session_id.clone(),
            self.agent_did.clone(),
            internal_call_id.to_string(),
            sequence,
            GET_GOAL_TOOL_NAME.to_string(),
            args.to_string(),
            deadline_at,
        )
        .with_requester_did(self.active_requester_did().await)
        .with_request_doc_id(self.active_request_doc_id().await);
        lifecycle.start_running().await?;
        let result = if serde_json::from_str::<GetGoalArgs>(args).is_err() {
            json!({"error": "get_goal expects an empty object"}).to_string()
        } else if let Some(mut goal) =
            load_canonical_goal(&self.node, &self.agent_did, &session_id).await?
        {
            refresh_goal_usage(&self.node, &goal).await?;
            goal = load_canonical_goal(&self.node, &self.agent_did, &session_id)
                .await?
                .unwrap_or(goal);
            serde_json::to_string_pretty(&GoalSnapshot::from_document(&goal, Utc::now()))?
        } else {
            json!({"goal": null}).to_string()
        };
        lifecycle.complete(&result).await?;
        Ok(self.skip_tool_result(GET_GOAL_TOOL_NAME, result))
    }

    pub(super) async fn persist_update_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, sequence) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );
        let mut lifecycle = ToolCallLifecycle::new(
            self.node.clone(),
            request_id.clone(),
            session_id.clone(),
            self.agent_did.clone(),
            internal_call_id.to_string(),
            sequence,
            UPDATE_GOAL_TOOL_NAME.to_string(),
            args.to_string(),
            deadline_at,
        )
        .with_requester_did(self.active_requester_did().await)
        .with_request_doc_id(self.active_request_doc_id().await);
        lifecycle.start_running().await?;
        let parsed = match serde_json::from_str::<UpdateGoalArgs>(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                let result =
                    json!({"error": format!("invalid update_goal arguments: {error}")}).to_string();
                lifecycle.complete(&result).await?;
                return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
            }
        };
        let Some(goal) = load_canonical_goal(&self.node, &self.agent_did, &session_id).await?
        else {
            let result = json!({"error": "the current session has no durable goal"}).to_string();
            lifecycle.complete(&result).await?;
            return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
        };

        let now = Utc::now();
        let updated_at = escape_graphql_string(&now.to_rfc3339());
        let Some(pre) = goal.state() else {
            let result =
                json!({"error": format!("durable goal has unknown status {:?}", goal.status)})
                    .to_string();
            lifecycle.complete(&result).await?;
            return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
        };
        let outcome = match parsed.status.trim() {
            "complete" => {
                let Some(post) = pre.step(GoalAction::Complete) else {
                    let result =
                        json!({"error": "complete is not legal from the goal's current status"})
                            .to_string();
                    lifecycle.complete(&result).await?;
                    return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
                };
                let active_time = goal.current_active_time_seconds(now);
                let reason = parsed
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .map(escape_graphql_string);
                let evidence_field = reason
                    .as_deref()
                    .map(|reason| format!(r#"completion_evidence: "{reason}","#))
                    .unwrap_or_else(|| "completion_evidence: null,".to_string());
                update_goal_fields(
                    &self.node,
                    &goal,
                    &format!(
                        r#"status: "{}", active_time_seconds: {active_time}, active_started_at: null, wrapup_completed: {}, last_failure: null, {evidence_field} updated_at: "{updated_at}""#,
                        post.status.as_str(), post.wrapup_completed
                    ),
                )
                .await?;
                json!({"accepted": true, "status": GoalStatus::Complete.as_str()})
            }
            "blocked" => {
                if pre.status != GoalStatus::Active {
                    let result = json!({
                        "error": format!(
                            "blocked audit is legal only from active; current status is {}",
                            pre.status.as_str()
                        )
                    })
                    .to_string();
                    lifecycle.complete(&result).await?;
                    return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
                }
                let Some(reason) = parsed
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                else {
                    let result = json!({"error": "blocked requires a non-empty reason identifying the repeated condition"}).to_string();
                    lifecycle.complete(&result).await?;
                    return Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result));
                };
                let (audits, accepted) = next_blocked_audit(
                    goal.consecutive_blocked_audits.unwrap_or_default(),
                    goal.last_blocked_reason.as_deref(),
                    goal.last_blocked_request_id.as_deref(),
                    reason,
                    &request_id,
                );
                let status = if accepted {
                    GoalStatus::Blocked
                } else {
                    GoalStatus::Active
                };
                let active_time = goal.current_active_time_seconds(now);
                let active_fields = if accepted {
                    format!("active_time_seconds: {active_time}, active_started_at: null,")
                } else {
                    String::new()
                };
                let reason = escape_graphql_string(reason);
                let request_id = escape_graphql_string(&request_id);
                update_goal_fields(
                    &self.node,
                    &goal,
                    &format!(
                        r#"status: "{}", consecutive_blocked_audits: {audits}, last_blocked_request_id: "{request_id}", last_blocked_reason: "{reason}", {active_fields} updated_at: "{updated_at}""#,
                        status.as_str()
                    ),
                )
                .await?;
                json!({
                    "accepted": accepted,
                    "status": status.as_str(),
                    "consecutive_blocked_audits": audits,
                    "required_blocked_audits": BLOCKED_AUDIT_THRESHOLD,
                })
            }
            other => json!({
                "error": format!("unsupported goal status {other:?}; expected complete or blocked")
            }),
        };
        let result = serde_json::to_string_pretty(&outcome)?;
        lifecycle.complete(&result).await?;
        Ok(self.skip_tool_result(UPDATE_GOAL_TOOL_NAME, result))
    }
}
