use chrono::Utc;
use serde_json::json;

use crate::goal::{
    create_goal_for_request_from_access, edit_active_goal_from_access, load_goal_tool_context,
    next_blocked_audit, refresh_goal_usage_from_access, update_goal_fields_if_status_from_access,
    CreateGoalForSessionError, GoalAction, GoalEdit, GoalState, RequestAssignmentObservation,
    BLOCKED_AUDIT_THRESHOLD,
};
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_protocol::goal_status::GoalStatus;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::hook::persistence::goal_tools::{
    GoalTool, GoalToolCall, GoalToolReply, GoalTools,
};
use gents_toolcalls::call::hook::persistence::helpers::json_string;
use gents_tools::native::{CreateGoalArgs, GetGoalArgs, UpdateGoalArgs};

/// The Goals plugin's answers to the Goal tools.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoalToolAnswers;

#[async_trait::async_trait]
impl GoalTools for GoalToolAnswers {
    async fn answer(
        &self,
        tool: GoalTool,
        call: &GoalToolCall<'_>,
    ) -> anyhow::Result<GoalToolReply> {
        match tool {
            GoalTool::Create => create_goal(call).await,
            GoalTool::Get => get_goal(call).await,
            GoalTool::Update => update_goal(call).await,
        }
    }
}

async fn create_goal(call: &GoalToolCall<'_>) -> anyhow::Result<GoalToolReply> {
    let (args, session_id, request_id) = (call.args(), call.session_id(), call.request_id());
    let parsed = match serde_json::from_str::<CreateGoalArgs>(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            let result = json_string(json!({
                "accepted": false,
                "disposition": "invalid",
                "error": format!("invalid create_goal arguments: {error}"),
            }));
            return Ok(GoalToolReply::Complete(result));
        }
    };
    let context = match load_goal_tool_context(
        &call.actor_access(),
        call.node_did(),
        session_id,
        request_id,
        call.request_doc_id(),
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            let result = json_string(
                json!({"accepted": false, "error": format!("Goal caller attribution unavailable: {error:#}")}),
            );
            return Ok(GoalToolReply::Fail(
                result,
                FailureClass::ServiceUnavailable,
            ));
        }
    };
    if matches!(context.assignment, RequestAssignmentObservation::Unknown) {
        let result = json_string(json!({
            "accepted": false,
            "disposition": "error",
            "error": "Goal caller attribution is unknown; continue from a verified request",
        }));
        return Ok(GoalToolReply::Fail(
            result,
            FailureClass::ServiceUnavailable,
        ));
    }
    let outcome = match create_goal_for_request_from_access(
        call.node(),
        Some(&call.access()?),
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
    let result = gents_session::session::output_render::render(
        &outcome,
        &["goal", "error", "accepted", "disposition"],
    )?;
    Ok(GoalToolReply::Complete(result))
}

async fn get_goal(call: &GoalToolCall<'_>) -> anyhow::Result<GoalToolReply> {
    let (args, session_id, request_id) = (call.args(), call.session_id(), call.request_id());
    let mut attribution_unavailable = false;
    let result = if serde_json::from_str::<GetGoalArgs>(args).is_err() {
        json_string(json!({"error": "get_goal expects an empty object"}))
    } else {
        let context = async {
            let mut context = load_goal_tool_context(
                &call.actor_access(),
                call.node_did(),
                session_id,
                request_id,
                call.request_doc_id(),
            )
            .await?;
            if context.can_write() {
                if let RequestAssignmentObservation::Assigned(goal) = &context.assignment {
                    refresh_goal_usage_from_access(&call.access()?, goal).await?;
                }
                context = load_goal_tool_context(
                    &call.actor_access(),
                    call.node_did(),
                    session_id,
                    request_id,
                    call.request_doc_id(),
                )
                .await?;
            }
            Ok::<_, anyhow::Error>(context)
        }
        .await;
        match context {
            Err(error) => {
                attribution_unavailable = true;
                json_string(
                    json!({"error": format!("Goal caller attribution unavailable: {error:#}")}),
                )
            }
            Ok(context) => {
                let read_only = !context.can_write();
                match context.assignment {
                    RequestAssignmentObservation::None => json_string(json!({"goal": null})),
                    RequestAssignmentObservation::Unknown => {
                        attribution_unavailable = true;
                        json_string(
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
                        gents_session::session::output_render::render(
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
        Ok(GoalToolReply::Fail(
            result,
            FailureClass::ServiceUnavailable,
        ))
    } else {
        Ok(GoalToolReply::Complete(result))
    }
}

async fn update_goal(call: &GoalToolCall<'_>) -> anyhow::Result<GoalToolReply> {
    let (args, session_id, request_id) = (call.args(), call.session_id(), call.request_id());
    let parsed = match serde_json::from_str::<UpdateGoalArgs>(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            let result =
                json_string(json!({"error": format!("invalid update_goal arguments: {error}")}));
            return Ok(GoalToolReply::Complete(result));
        }
    };
    let context = match load_goal_tool_context(
        &call.actor_access(),
        call.node_did(),
        session_id,
        request_id,
        call.request_doc_id(),
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            let result = json_string(
                json!({"accepted": false, "error": format!("Goal caller attribution unavailable: {error:#}")}),
            );
            return Ok(GoalToolReply::Fail(
                result,
                FailureClass::ServiceUnavailable,
            ));
        }
    };
    if !context.can_write() {
        let unavailable = matches!(context.assignment, RequestAssignmentObservation::Unknown);
        let error = match context.assignment {
                RequestAssignmentObservation::None => "the calling request has no Goal assignment",
                RequestAssignmentObservation::Unknown => "Goal caller attribution is unknown; continue from a verified request",
                RequestAssignmentObservation::Assigned(_) => "the calling request's retained Goal assignment is read-only; a new request is required to work on the current Goal",
            };
        let result = json_string(json!({"accepted": false, "error": error}));
        if unavailable {
            return Ok(GoalToolReply::Fail(
                result,
                FailureClass::ServiceUnavailable,
            ));
        }
        return Ok(GoalToolReply::Complete(result));
    }
    let RequestAssignmentObservation::Assigned(goal) = context.assignment else {
        unreachable!("only a current assigned caller can update a Goal");
    };

    let now = Utc::now();
    let updated_at = escape_graphql_string(&now.to_rfc3339());
    let Some(pre) = goal.state() else {
        let result = json_string(
            json!({"error": format!("durable goal has unknown status {:?}", goal.status)}),
        );
        return Ok(GoalToolReply::Complete(result));
    };
    let route = update_route(
        parsed.status.as_deref(),
        parsed.objective.is_some() || parsed.token_budget.is_some(),
        parsed.reason.as_deref(),
        pre,
    );
    let outcome = match route {
        UpdateRoute::ReasonOutsideCompletion => {
            json!({"accepted": false, "error": "reason is only accepted with status complete or blocked"})
        }
        UpdateRoute::NotActiveResumable | UpdateRoute::NotActiveFinal => {
            let hint = match (route, pre.status) {
                (UpdateRoute::NotActiveResumable, _) => {
                    "an operator resumes it with `gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)"
                }
                (_, GoalStatus::Complete) => "a complete goal is terminal and has no resume path",
                (_, GoalStatus::BudgetLimited) => {
                    "a budget_limited goal cannot be reactivated and has no resume path"
                }
                _ => "the goal was removed by its operator and cannot be resumed",
            };
            json!({
                "accepted": false,
                "error": format!(
                    "update_goal edits require an active goal; the current goal status is {} and {}",
                    pre.status.as_str(),
                    hint
                ),
            })
        }
        UpdateRoute::Edit => {
            let fields = GoalEdit {
                objective: parsed.objective,
                budget: parsed.token_budget,
            };
            if let Err(error) = fields.validate() {
                json!({"accepted": false, "error": error.to_string()})
            } else if let Some(updated) =
                edit_active_goal_from_access(&call.access()?, &goal, &fields).await?
            {
                json!({"accepted": true, "goal": crate::goal::snapshot(&updated, Utc::now())})
            } else {
                json!({"accepted": false, "error": "goal is no longer active or its continuation changed; reload with get_goal"})
            }
        }
        UpdateRoute::FieldsOutsideActive => {
            json!({"accepted": false, "error": "objective and token_budget can only be edited while status is active"})
        }
        UpdateRoute::Complete | UpdateRoute::CompleteIllegal => {
            let Some(post) = pre.step(GoalAction::Complete) else {
                let result = json_string(
                    json!({"error": "complete is not legal from the goal's current status"}),
                );
                return Ok(GoalToolReply::Complete(result));
            };
            if let Some(gate) = call.output_obligation_gate() {
                let unmet = gate.unmet().await?;
                if !unmet.is_empty() {
                    let result = json_string(json!({
                        "accepted": false,
                        "error": gents_loop::output_obligation::continuation_message(&unmet),
                    }));
                    return Ok(GoalToolReply::Complete(result));
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
            let applied = update_goal_fields_if_status_from_access(
                    &call.access()?,
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
        UpdateRoute::BlockedNotActive => {
            let result = json_string(json!({
                "error": format!(
                    "blocked audit is legal only from active; current status is {}",
                    pre.status.as_str()
                )
            }));
            return Ok(GoalToolReply::Complete(result));
        }
        UpdateRoute::BlockedNeedsReason => {
            let result = json_string(
                json!({"error": "blocked requires a non-empty reason identifying the repeated condition"}),
            );
            return Ok(GoalToolReply::Complete(result));
        }
        UpdateRoute::BlockedAudit => {
            let reason = parsed.reason.as_deref().map(str::trim).unwrap_or_default();
            let (audits, accepted) = next_blocked_audit(
                goal.consecutive_blocked_audits.unwrap_or_default(),
                goal.last_blocked_reason.as_deref(),
                goal.last_blocked_request_id.as_deref(),
                reason,
                request_id,
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
            let applied = update_goal_fields_if_status_from_access(
                    &call.access()?,
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
        UpdateRoute::Unsupported => json!({
            "error": format!(
                "unsupported goal status {:?}; expected active, complete or blocked",
                parsed.status.as_deref().map(str::trim).unwrap_or_default()
            )
        }),
    };
    let result = gents_session::session::output_render::render(
        &outcome,
        &["goal", "error", "accepted", "disposition"],
    )?;
    Ok(GoalToolReply::Complete(result))
}

/// What `update_goal` does with a call from the Goal's current assigned
/// caller (Lean `GoalAutomation.UpdateGoal.route`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum UpdateRoute {
    Edit,
    ReasonOutsideCompletion,
    NotActiveResumable,
    NotActiveFinal,
    FieldsOutsideActive,
    Complete,
    CompleteIllegal,
    BlockedAudit,
    BlockedNotActive,
    BlockedNeedsReason,
    Unsupported,
}

fn update_route(
    requested: Option<&str>,
    fields: bool,
    reason: Option<&str>,
    pre: GoalState,
) -> UpdateRoute {
    let reason_given = reason.map(str::trim).is_some_and(|reason| !reason.is_empty());
    match requested.map(str::trim).unwrap_or("active") {
        "active" if reason.is_some() => UpdateRoute::ReasonOutsideCompletion,
        "active" if pre.status == GoalStatus::Active => UpdateRoute::Edit,
        "active" if pre.step(GoalAction::Resume).is_some() => UpdateRoute::NotActiveResumable,
        "active" => UpdateRoute::NotActiveFinal,
        "complete" | "blocked" if fields => UpdateRoute::FieldsOutsideActive,
        "complete" if pre.step(GoalAction::Complete).is_some() => UpdateRoute::Complete,
        "complete" => UpdateRoute::CompleteIllegal,
        "blocked" if pre.status != GoalStatus::Active => UpdateRoute::BlockedNotActive,
        "blocked" if !reason_given => UpdateRoute::BlockedNeedsReason,
        "blocked" => UpdateRoute::BlockedAudit,
        _ => UpdateRoute::Unsupported,
    }
}

#[cfg(test)]
mod update_route_conformance {
    use super::{update_route, GoalState, GoalStatus};
    use gents_lean_vocab::families::goal_automation_update_goal_route as family;

    #[test]
    fn update_route_matches_lean() {
        family::replay(|case: &family::Input<String, String, GoalStatus>| {
            let requested = match case.requested.as_str() {
                "omitted" => None,
                "unsupported" => Some("paused"),
                status => Some(status),
            };
            let reason = match case.reason.as_str() {
                "absent" => None,
                "blank" => Some("  "),
                _ => Some("needs approval"),
            };
            let pre = GoalState {
                status: case.status,
                blocked_audits: 0,
                wrapup_requested: false,
                wrapup_completed: false,
            };
            update_route(requested, case.fields, reason, pre)
        });
    }
}
