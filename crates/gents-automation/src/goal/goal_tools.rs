use chrono::Utc;
use serde_json::json;

use crate::goal::{
    create_goal_for_request_from_access, load_goal_tool_context, refresh_goal_usage_from_access,
    CreateGoalForSessionError, RequestAssignmentObservation, BLOCKED_AUDIT_THRESHOLD,
};
use crate::operations::goal::set_as_working_agent;
use gents_protocol::operations::goal::{GoalOperationError, GoalSetInput};
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_protocol::goal_status::GoalStatus;
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

    let status = match parsed.status.as_deref().map(str::trim) {
        None => None,
        Some(name) => match GoalStatus::parse(name) {
            Some(status) => Some(status),
            None => {
                let result = json_string(json!({
                    "accepted": false,
                    "error": format!(
                        "unsupported goal status {name:?}; expected active, complete or blocked"
                    ),
                }));
                return Ok(GoalToolReply::Complete(result));
            }
        },
    };
    let unmet = match (status, call.output_obligation_gate()) {
        (Some(GoalStatus::Complete), Some(gate)) => {
            let unmet = gate.unmet().await?;
            (!unmet.is_empty())
                .then(|| gents_loop::output_obligation::continuation_message(&unmet))
        }
        _ => None,
    };
    let input = GoalSetInput {
        objective: parsed.objective,
        token_budget: parsed.token_budget.map(Some),
        status,
        reason: parsed.reason,
        ..Default::default()
    };
    let outcome = match write_through_handler(call, &goal, &input, unmet.as_deref()).await? {
        Err(answer) => answer,
        Ok(updated) => match status {
            Some(GoalStatus::Complete) => {
                json!({"accepted": true, "status": GoalStatus::Complete.as_str()})
            }
            Some(GoalStatus::Blocked) => json!({
                "accepted": updated.parsed_status() == Some(GoalStatus::Blocked),
                "status": updated.status,
                "consecutive_blocked_audits": updated.consecutive_blocked_audits.unwrap_or_default(),
                "required_blocked_audits": BLOCKED_AUDIT_THRESHOLD,
            }),
            _ => json!({"accepted": true, "goal": crate::goal::snapshot(&updated, Utc::now())}),
        },
    };
    let result = gents_session::session::output_render::render(
        &outcome,
        &["goal", "error", "accepted", "disposition"],
    )?;
    Ok(GoalToolReply::Complete(result))
}

/// Writes an `update_goal` call through the Goal handler as the working
/// Agent; a refusal becomes the tool's answer.
async fn write_through_handler(
    call: &GoalToolCall<'_>,
    goal: &crate::goal::GoalDocument,
    input: &GoalSetInput,
    obligations_unmet: Option<&str>,
) -> anyhow::Result<Result<crate::goal::GoalDocument, serde_json::Value>> {
    match set_as_working_agent(
        &call.access()?,
        goal,
        input,
        obligations_unmet,
        call.request_id(),
    )
    .await
    {
        Ok(updated) => Ok(Ok(updated)),
        Err(GoalOperationError::Passthrough(error)) => Err(anyhow::anyhow!(error)),
        Err(GoalOperationError::Stale) => Ok(Err(
            json!({"accepted": false, "error": "goal changed since this request observed it; reload with get_goal"}),
        )),
        Err(refusal) => Ok(Err(json!({"accepted": false, "error": refusal.to_string()}))),
    }
}

#[cfg(test)]
mod tests;
