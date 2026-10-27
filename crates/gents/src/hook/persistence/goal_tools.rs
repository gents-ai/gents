use chrono::Utc;
use serde_json::json;

use crate::goal::{
    create_goal_for_request, edit_active_goal, load_goal_tool_context, next_blocked_audit,
    refresh_goal_usage, update_goal_fields_if_status, CreateGoalForSessionError, GoalAction,
    GoalEdit, GoalStatus, RequestAssignmentObservation, BLOCKED_AUDIT_THRESHOLD,
    CREATE_GOAL_TOOL_NAME, GET_GOAL_TOOL_NAME, UPDATE_GOAL_TOOL_NAME,
};
use crate::graphql::escape_graphql_string;
use crate::llm::ToolCallHookAction;
use crate::toolset::{CreateGoalArgs, GetGoalArgs, UpdateGoalArgs};

use super::DefraSessionHook;

impl DefraSessionHook {
    pub(super) async fn persist_create_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _sequence) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                CREATE_GOAL_TOOL_NAME,
                args,
                deadline_at,
                crate::tool_call_lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let parsed = match serde_json::from_str::<CreateGoalArgs>(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                let result = super::helpers::json_string(json!({
                    "accepted": false,
                    "disposition": "invalid",
                    "error": format!("invalid create_goal arguments: {error}"),
                }));
                return self
                    .complete_control_tool_call(&mut lifecycle, CREATE_GOAL_TOOL_NAME, result)
                    .await;
            }
        };
        let context = match load_goal_tool_context(
            &self.node,
            &self.node_did,
            &session_id,
            &request_id,
            lifecycle.request_doc_id(),
        )
        .await
        {
            Ok(context) => context,
            Err(error) => {
                let result = super::helpers::json_string(
                    json!({"accepted": false, "error": format!("Goal caller attribution unavailable: {error:#}")}),
                );
                return self
                    .fail_control_tool_call(
                        &mut lifecycle,
                        CREATE_GOAL_TOOL_NAME,
                        result,
                        crate::tool_call_lifecycle::FailureClass::ServiceUnavailable,
                    )
                    .await;
            }
        };
        let outcome = match create_goal_for_request(
            &self.node,
            &context.request,
            &parsed.objective,
            parsed.token_budget,
        )
        .await
        {
            Ok(outcome) => json!({
                "accepted": true,
                "disposition": outcome.disposition(),
                "goal": crate::goal::snapshot(outcome.goal(), Utc::now()),
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
        let result =
            crate::tool_output::render(&outcome, &["goal", "error", "accepted", "disposition"])?;
        self.complete_control_tool_call(&mut lifecycle, CREATE_GOAL_TOOL_NAME, result)
            .await
    }

    pub(super) async fn persist_get_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _sequence) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                GET_GOAL_TOOL_NAME,
                args,
                deadline_at,
                crate::tool_call_lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let mut attribution_unavailable = false;
        let result = if serde_json::from_str::<GetGoalArgs>(args).is_err() {
            super::helpers::json_string(json!({"error": "get_goal expects an empty object"}))
        } else {
            let context = async {
                let mut context = load_goal_tool_context(
                    &self.node,
                    &self.node_did,
                    &session_id,
                    &request_id,
                    lifecycle.request_doc_id(),
                )
                .await?;
                if context.can_write() {
                    if let RequestAssignmentObservation::Assigned(goal) = &context.assignment {
                        refresh_goal_usage(&self.node, goal).await?;
                    }
                    context = load_goal_tool_context(
                        &self.node,
                        &self.node_did,
                        &session_id,
                        &request_id,
                        lifecycle.request_doc_id(),
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(context)
            }
            .await;
            match context {
                Err(error) => {
                    attribution_unavailable = true;
                    super::helpers::json_string(
                        json!({"error": format!("Goal caller attribution unavailable: {error:#}")}),
                    )
                }
                Ok(context) => {
                    let read_only = !context.can_write();
                    match context.assignment {
                        RequestAssignmentObservation::None => {
                            super::helpers::json_string(json!({"goal": null}))
                        }
                        RequestAssignmentObservation::Unknown => {
                            attribution_unavailable = true;
                            super::helpers::json_string(
                                json!({"error": "Goal caller attribution is unknown; continue from a verified request"}),
                            )
                        }
                        RequestAssignmentObservation::Assigned(goal) => {
                            let mut observed = crate::goal::snapshot(&goal, Utc::now());
                            if read_only {
                                observed.active_time_seconds =
                                    goal.active_time_seconds.unwrap_or_default().max(0);
                            }
                            let mut value = serde_json::to_value(observed)?;
                            value["read_only"] = json!(read_only);
                            if read_only {
                                value["assignment_context"] =
                                    json!(if goal.parsed_status() == Some(GoalStatus::Active) {
                                        "superseded"
                                    } else {
                                        "retired"
                                    });
                                value["notice"] = json!("This is the calling request's retained assignment. It is read-only; a new request is required to work on the current Goal.");
                            }
                            crate::tool_output::render(
                                &value,
                                &[
                                    "objective",
                                    "status",
                                    "read_only",
                                    "assignment_context",
                                    "notice",
                                    "tokens_used",
                                    "token_budget",
                                    "active_time_seconds",
                                ],
                            )?
                        }
                    }
                }
            }
        };
        if attribution_unavailable {
            self.fail_control_tool_call(
                &mut lifecycle,
                GET_GOAL_TOOL_NAME,
                result,
                crate::tool_call_lifecycle::FailureClass::ServiceUnavailable,
            )
            .await
        } else {
            self.complete_control_tool_call(&mut lifecycle, GET_GOAL_TOOL_NAME, result)
                .await
        }
    }

    pub(super) async fn persist_update_goal_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _sequence) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                UPDATE_GOAL_TOOL_NAME,
                args,
                deadline_at,
                crate::tool_call_lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let parsed = match serde_json::from_str::<UpdateGoalArgs>(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                let result = super::helpers::json_string(
                    json!({"error": format!("invalid update_goal arguments: {error}")}),
                );
                return self
                    .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                    .await;
            }
        };
        let context = match load_goal_tool_context(
            &self.node,
            &self.node_did,
            &session_id,
            &request_id,
            lifecycle.request_doc_id(),
        )
        .await
        {
            Ok(context) => context,
            Err(error) => {
                let result = super::helpers::json_string(
                    json!({"accepted": false, "error": format!("Goal caller attribution unavailable: {error:#}")}),
                );
                return self
                    .fail_control_tool_call(
                        &mut lifecycle,
                        UPDATE_GOAL_TOOL_NAME,
                        result,
                        crate::tool_call_lifecycle::FailureClass::ServiceUnavailable,
                    )
                    .await;
            }
        };
        if !context.can_write() {
            let error = match context.assignment {
                RequestAssignmentObservation::None => "the calling request has no Goal assignment",
                RequestAssignmentObservation::Unknown => "Goal caller attribution is unknown; continue from a verified request",
                RequestAssignmentObservation::Assigned(_) => "the calling request's retained Goal assignment is read-only; a new request is required to work on the current Goal",
            };
            let result = super::helpers::json_string(json!({"accepted": false, "error": error}));
            return self
                .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                .await;
        }
        let RequestAssignmentObservation::Assigned(goal) = context.assignment else {
            unreachable!("only a current assigned caller can update a Goal");
        };

        let now = Utc::now();
        let updated_at = escape_graphql_string(&now.to_rfc3339());
        let Some(pre) = goal.state() else {
            let result = super::helpers::json_string(
                json!({"error": format!("durable goal has unknown status {:?}", goal.status)}),
            );
            return self
                .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                .await;
        };
        let outcome = match parsed.status.as_deref().map(str::trim).unwrap_or("active") {
            "active" => {
                let fields = GoalEdit {
                    objective: parsed.objective,
                    budget: parsed.token_budget,
                };
                if parsed.reason.is_some() {
                    json!({"accepted": false, "error": "reason is only accepted with status complete or blocked"})
                } else if pre.status != GoalStatus::Active {
                    let hint = match pre.status {
                        GoalStatus::Active => unreachable!("active goals use the edit path"),
                        GoalStatus::Blocked | GoalStatus::Paused | GoalStatus::UsageLimited => {
                            "an operator resumes it with `gents goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)"
                        }
                        GoalStatus::Complete => {
                            "a complete goal is terminal and has no resume path"
                        }
                        GoalStatus::Stopped => {
                            "the goal was removed by its operator and cannot be resumed"
                        }
                        GoalStatus::BudgetLimited => {
                            "a budget_limited goal cannot be reactivated and has no resume path"
                        }
                    };
                    json!({
                        "accepted": false,
                        "error": format!(
                            "update_goal edits require an active goal; the current goal status is {} and {}",
                            pre.status.as_str(),
                            hint
                        ),
                    })
                } else if let Err(error) = fields.validate() {
                    json!({"accepted": false, "error": error.to_string()})
                } else if let Some(updated) = edit_active_goal(&self.node, &goal, &fields).await? {
                    json!({"accepted": true, "goal": crate::goal::snapshot(&updated, Utc::now())})
                } else {
                    json!({"accepted": false, "error": "goal is no longer active or its continuation changed; reload with get_goal"})
                }
            }
            "complete" | "blocked"
                if parsed.objective.is_some() || parsed.token_budget.is_some() =>
            {
                json!({"accepted": false, "error": "objective and token_budget can only be edited while status is active"})
            }
            "complete" => {
                let Some(post) = pre.step(GoalAction::Complete) else {
                    let result = super::helpers::json_string(
                        json!({"error": "complete is not legal from the goal's current status"}),
                    );
                    return self
                        .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                        .await;
                };
                if let Some(gate) = self.output_obligation_gate.as_ref() {
                    let unmet = gate.unmet().await?;
                    if !unmet.is_empty() {
                        let result = super::helpers::json_string(json!({
                            "accepted": false,
                            "error": crate::agent::output_obligation::continuation_message(&unmet),
                        }));
                        return self
                            .complete_control_tool_call(
                                &mut lifecycle,
                                UPDATE_GOAL_TOOL_NAME,
                                result,
                            )
                            .await;
                    }
                }
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
                let applied = update_goal_fields_if_status(
                    &self.node,
                    &goal,
                    pre.status,
                    &format!(
                        r#"status: "{}", active_time_seconds: {active_time}, active_started_at: null, wrapup_completed: {}, last_failure: null, {evidence_field} updated_at: "{updated_at}""#,
                        post.status.as_str(), post.wrapup_completed
                    ),
                )
                .await?;
                if applied {
                    json!({"accepted": true, "status": GoalStatus::Complete.as_str()})
                } else {
                    json!({"accepted": false, "error": "goal changed while applying completion; reload its durable state"})
                }
            }
            "blocked" => {
                if pre.status != GoalStatus::Active {
                    let result = super::helpers::json_string(json!({
                        "error": format!(
                            "blocked audit is legal only from active; current status is {}",
                            pre.status.as_str()
                        )
                    }));
                    return self
                        .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                        .await;
                }
                let Some(reason) = parsed
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                else {
                    let result = super::helpers::json_string(
                        json!({"error": "blocked requires a non-empty reason identifying the repeated condition"}),
                    );
                    return self
                        .complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
                        .await;
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
                let applied = update_goal_fields_if_status(
                    &self.node,
                    &goal,
                    pre.status,
                    &format!(
                        r#"status: "{}", consecutive_blocked_audits: {audits}, last_blocked_request_id: "{request_id}", last_blocked_reason: "{reason}", {active_fields} updated_at: "{updated_at}""#,
                        status.as_str()
                    ),
                )
                .await?;
                if applied {
                    json!({
                        "accepted": accepted,
                        "status": status.as_str(),
                        "consecutive_blocked_audits": audits,
                        "required_blocked_audits": BLOCKED_AUDIT_THRESHOLD,
                    })
                } else {
                    json!({"accepted": false, "error": "goal changed while applying blocked audit; reload its durable state"})
                }
            }
            other => json!({
                "error": format!("unsupported goal status {other:?}; expected active, complete or blocked")
            }),
        };
        let result =
            crate::tool_output::render(&outcome, &["goal", "error", "accepted", "disposition"])?;
        self.complete_control_tool_call(&mut lifecycle, UPDATE_GOAL_TOOL_NAME, result)
            .await
    }
}
