//! Lean `GoalAutomation.Operations`: starting and setting a session's Goal,
//! for every client. The CLI, the desktop and editor through their signed
//! command, the shell and the working Agent's Goal tools all decide here.

use anyhow::Context;
use chrono::Utc;
use gents_protocol::goal_control::GoalRevision;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::operations::goal::{
    GoalActor, GoalClaim, GoalExisting, GoalExpected, GoalFieldChange, GoalOperationError,
    GoalSession, GoalSetDecision, GoalSetInput, GoalStartDecision, GoalStartInput, GoalTarget,
};
use gents_protocol::operations::inference::InferenceActor;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

use crate::goal::{
    goal_session_is_idle, load_canonical_goal_in_txn, load_goal_creation_claim_for_key_in_txn,
    load_session_requests, same_goal_assignment, GoalAction, GoalAuditObservation,
    GoalBackedRequestDisposition, GoalCreationFingerprint, GoalDocument, GoalState,
};

type Refusal<T> = std::result::Result<T, GoalOperationError>;

/// Lean `GoalAutomation.Operations.startDecision`.
pub fn start_decision(
    actor: GoalActor,
    session: GoalSession,
    existing: GoalExisting,
    expected: bool,
    claim: GoalClaim,
) -> GoalStartDecision {
    use GoalStartDecision as D;
    match actor {
        GoalActor::OtherAgent => D::Denied,
        GoalActor::WorkingAgent => {
            if session == GoalSession::Missing {
                return D::SessionMissing;
            }
            match (existing, claim) {
                (GoalExisting::None, GoalClaim::None) => D::Fresh,
                (_, GoalClaim::Same) => D::Idempotent,
                _ => D::Conflict,
            }
        }
        GoalActor::Person => {
            if session == GoalSession::Missing {
                return D::SessionMissing;
            }
            match existing {
                GoalExisting::Live => D::LiveGoal,
                GoalExisting::Finished if !expected => D::Stale,
                GoalExisting::Finished if session == GoalSession::Busy => D::Busy,
                GoalExisting::Finished => D::Replace,
                GoalExisting::None if expected => D::Stale,
                GoalExisting::None if session == GoalSession::Busy => D::Busy,
                GoalExisting::None => match claim {
                    GoalClaim::None => D::Fresh,
                    GoalClaim::Same => D::Idempotent,
                    GoalClaim::Different => D::Conflict,
                },
            }
        }
    }
}

/// Lean `GoalAutomation.Edit.editable` through `Operations.origin`.
fn editable(actor: GoalActor, status: GoalStatus) -> bool {
    status == GoalStatus::Active || actor == GoalActor::Person && status == GoalStatus::Paused
}

/// Lean `GoalAutomation.Operations.setDecision`.
pub fn set_decision(
    actor: GoalActor,
    status: GoalStatus,
    target: GoalTarget,
    fields: GoalFieldChange,
    auto_resume: bool,
    obligations_met: bool,
) -> GoalSetDecision {
    use GoalSetDecision as D;
    let working = actor == GoalActor::WorkingAgent;
    let agent_target = matches!(
        target,
        GoalTarget::Keep | GoalTarget::Activate | GoalTarget::Complete | GoalTarget::Block
    );
    let changes_status = !matches!(target, GoalTarget::Keep | GoalTarget::Activate);
    if actor == GoalActor::OtherAgent {
        D::Denied
    } else if target == GoalTarget::Keep && fields == GoalFieldChange::None && !auto_resume {
        D::NothingToChange
    } else if working && (auto_resume || fields == GoalFieldChange::ClearBudget || !agent_target) {
        D::PersonOnly
    } else if working && fields != GoalFieldChange::None && changes_status {
        D::FieldsNeedKeep
    } else if fields != GoalFieldChange::None && !editable(actor, status) {
        D::NotEditable
    } else if target == GoalTarget::Activate && status != GoalStatus::Active {
        D::ResumeRequired
    } else if working && target == GoalTarget::Complete && !obligations_met {
        D::ObligationsUnmet
    } else {
        D::Apply
    }
}

fn actor_of(kind: InferenceActor, actor: Option<&str>, session_agent: &str) -> GoalActor {
    match kind {
        InferenceActor::Person => GoalActor::Person,
        InferenceActor::Agent if actor == Some(session_agent) => GoalActor::WorkingAgent,
        InferenceActor::Agent => GoalActor::OtherAgent,
    }
}

/// Who `txn` acts as toward the session whose Agent is `session_agent`.
pub async fn acting_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_agent: &str,
) -> anyhow::Result<GoalActor> {
    let kind = gents_inference::operations::acting(txn, node_did).await?;
    Ok(actor_of(kind, txn.actor_did(), session_agent))
}

/// Who the author of a command acts as, once the caller verified its
/// signature.
pub async fn acting_as_signer(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_agent: &str,
    signer: &str,
) -> anyhow::Result<GoalActor> {
    let kind = gents_inference::operations::acting_signed_by(txn, node_did, signer).await?;
    Ok(actor_of(kind, Some(signer), session_agent))
}

/// A start the handler admitted, with the session's current Goal.
pub struct GoalStartAdmission {
    pub decision: GoalStartDecision,
    pub existing: Option<GoalDocument>,
}

fn validate_objective_and_budget(
    objective: Option<&str>,
    token_budget: Option<i64>,
) -> Refusal<()> {
    if objective.is_some_and(|objective| objective.trim().is_empty()) {
        return Err(GoalOperationError::InvalidObjective);
    }
    if token_budget.is_some_and(|budget| budget <= 0) {
        return Err(GoalOperationError::InvalidBudget);
    }
    Ok(())
}

async fn observed_revision_is_current(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    expected: &GoalRevision,
) -> anyhow::Result<bool> {
    if expected.document != goal.doc_id {
        return Ok(false);
    }
    let current = gents_store::graphql::newest_document_composite_commit_in_txn(
        txn,
        &goal.doc_id,
        "goal.operation_revision",
    )
    .await?
    .context("goal has no unambiguous composite revision")?;
    Ok(expected.composite == current.cid)
}

/// Decides whether `actor` may start a Goal on the session now. `claim_key`
/// names the creation claim the start would converge on, when it is not the
/// session's deterministic one.
#[allow(clippy::too_many_arguments)]
pub async fn admit_start_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: GoalActor,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    expected: Option<&GoalRevision>,
    claim_key: Option<&str>,
) -> anyhow::Result<Refusal<GoalStartAdmission>> {
    if let Err(refusal) = validate_objective_and_budget(Some(objective), token_budget) {
        return Ok(Err(refusal));
    }
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?;
    let existing = load_canonical_goal_in_txn(txn, node_did, session_id).await?;
    let (session_fact, claim) = match session {
        None => (GoalSession::Missing, GoalClaim::None),
        Some(_) => {
            let requests = load_session_requests(txn, node_did, session_id).await?;
            let idle = if goal_session_is_idle(&requests) {
                GoalSession::Idle
            } else {
                GoalSession::Busy
            };
            let requested = GoalCreationFingerprint {
                owner: node_did.to_owned(),
                session: session_id.to_owned(),
                objective: objective.trim().to_owned(),
                token_budget: token_budget.map(i128::from),
            };
            let recorded =
                match load_goal_creation_claim_for_key_in_txn(txn, node_did, session_id, claim_key)
                    .await?
                {
                    Some(claim) => Some(claim),
                    None => existing.as_ref().map(|goal| GoalCreationFingerprint {
                        owner: node_did.to_owned(),
                        session: session_id.to_owned(),
                        objective: goal.objective.trim().to_owned(),
                        token_budget: goal.token_budget.map(i128::from),
                    }),
                };
            let claim = match recorded {
                None => GoalClaim::None,
                Some(recorded) if recorded == requested => GoalClaim::Same,
                Some(_) => GoalClaim::Different,
            };
            (idle, claim)
        }
    };
    if let (Some(expected), Some(goal)) = (expected, existing.as_ref()) {
        if !observed_revision_is_current(txn, goal, expected).await? {
            return Ok(Err(GoalOperationError::Stale));
        }
    }
    let decision = start_decision(
        actor,
        session_fact,
        GoalExisting::of(existing.as_ref().and_then(GoalDocument::parsed_status)),
        expected.is_some(),
        claim,
    );
    Ok(match decision {
        GoalStartDecision::Fresh | GoalStartDecision::Idempotent | GoalStartDecision::Replace => {
            Ok(GoalStartAdmission { decision, existing })
        }
        GoalStartDecision::Denied => Err(GoalOperationError::Denied),
        GoalStartDecision::SessionMissing => {
            Err(GoalOperationError::SessionMissing(session_id.to_owned()))
        }
        GoalStartDecision::Busy => Err(GoalOperationError::Busy),
        GoalStartDecision::LiveGoal => Err(GoalOperationError::LiveGoal),
        GoalStartDecision::Stale => Err(GoalOperationError::Stale),
        GoalStartDecision::Conflict => Err(GoalOperationError::Conflict),
    })
}

/// Starts the session's Goal with its first request, which the client signed,
/// as whoever `access` acts for. The Goal and the request commit together.
pub async fn start(
    access: &ConfigAccess,
    node_did: &str,
    input: &GoalStartInput,
    request: &AgentRequestCreate,
) -> Result<GoalBackedRequestDisposition, GoalOperationError> {
    crate::goal::submit_goal_backed_request(
        access,
        node_did,
        &input.session_id,
        &input.objective,
        input.token_budget,
        request,
    )
    .await
    .map_err(passthrough)?
}

/// A set the handler admitted: the observed Goal and the state it moves to.
pub struct GoalSetPlan {
    goal: GoalDocument,
    objective: Option<String>,
    token_budget: Option<Option<i64>>,
    post: GoalState,
    auto_resume: Option<bool>,
    completion_evidence: Option<Option<String>>,
    blocked: Option<(String, String)>,
}

fn target_name(target: GoalTarget) -> &'static str {
    match target {
        GoalTarget::Keep => "keep",
        GoalTarget::Activate => "active",
        GoalTarget::Pause => "paused",
        GoalTarget::Stop => "stopped",
        GoalTarget::Complete => "complete",
        GoalTarget::Block => "blocked",
        GoalTarget::UsageLimited => "usage_limited",
        GoalTarget::BudgetLimited => "budget_limited",
    }
}

fn audit_observation(goal: &GoalDocument, reason: &str, request_id: &str) -> GoalAuditObservation {
    if goal.last_blocked_request_id.as_deref() == Some(request_id) {
        GoalAuditObservation::SameRequest
    } else if goal.last_blocked_reason.as_deref() == Some(reason) {
        GoalAuditObservation::SameCondition
    } else {
        GoalAuditObservation::NewCondition
    }
}

/// Decides a set on the session's current Goal. `obligations_unmet` is the
/// working Agent's unmet output obligations, as text it reads, when it asks to
/// complete; `request_id` is its calling request.
pub async fn admit_set_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: GoalActor,
    goal: &GoalDocument,
    input: &GoalSetInput,
    obligations_unmet: Option<&str>,
    request_id: Option<&str>,
) -> anyhow::Result<Refusal<GoalSetPlan>> {
    let current = match input.expected.as_ref() {
        None => true,
        Some(GoalExpected::Goal(goal_id)) => *goal_id == goal.goal_id,
        Some(GoalExpected::Assignment {
            goal_doc_id,
            goal_id,
            assignment_root_request_doc_id,
            status,
            continuation_sequence,
        }) => {
            *goal_doc_id == goal.doc_id
                && *assignment_root_request_doc_id == goal.assignment_root_request_doc_id
                && *goal_id == goal.goal_id
                && Some(*status) == goal.parsed_status()
                && *continuation_sequence == goal.continuation_sequence()
        }
        Some(GoalExpected::Revision(revision)) => {
            observed_revision_is_current(txn, goal, revision).await?
        }
    };
    if !current {
        return Ok(Err(GoalOperationError::Stale));
    }
    Ok(plan_set(actor, goal, input, obligations_unmet, request_id))
}

fn plan_set(
    actor: GoalActor,
    goal: &GoalDocument,
    input: &GoalSetInput,
    obligations_unmet: Option<&str>,
    request_id: Option<&str>,
) -> Refusal<GoalSetPlan> {
    validate_objective_and_budget(input.objective.as_deref(), input.token_budget.flatten())?;
    let target = input.target();
    let reason = input
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty());
    if input.reason.is_some() && !matches!(target, GoalTarget::Complete | GoalTarget::Block) {
        return Err(GoalOperationError::ReasonNotAccepted);
    }
    let pre = goal.state().ok_or(GoalOperationError::IllegalTransition {
        target: target_name(target),
        status: "unknown",
    })?;
    let status = pre.status.as_str();
    match set_decision(
        actor,
        pre.status,
        target,
        input.field_change(),
        input.auto_resume.is_some(),
        obligations_unmet.is_none(),
    ) {
        GoalSetDecision::Apply => {}
        GoalSetDecision::Denied => return Err(GoalOperationError::Denied),
        GoalSetDecision::NothingToChange => return Err(GoalOperationError::NothingToChange),
        GoalSetDecision::PersonOnly => return Err(GoalOperationError::PersonOnly),
        GoalSetDecision::FieldsNeedKeep => return Err(GoalOperationError::FieldsNeedKeep),
        GoalSetDecision::NotEditable => return Err(GoalOperationError::NotEditable(pre.status)),
        GoalSetDecision::ResumeRequired => {
            return Err(GoalOperationError::ResumeRequired(pre.status))
        }
        GoalSetDecision::ObligationsUnmet => {
            return Err(GoalOperationError::ObligationsUnmet(
                obligations_unmet.unwrap_or_default().to_owned(),
            ))
        }
    }
    let mut blocked = None;
    let action = match target {
        GoalTarget::Keep | GoalTarget::Activate => None,
        GoalTarget::Pause => Some(GoalAction::Pause),
        GoalTarget::Stop => Some(GoalAction::Stop),
        GoalTarget::Complete => Some(GoalAction::Complete),
        GoalTarget::UsageLimited => Some(GoalAction::UsageLimit),
        GoalTarget::BudgetLimited => Some(GoalAction::BudgetExhausted),
        GoalTarget::Block if actor == GoalActor::Person => Some(GoalAction::OperatorBlock),
        GoalTarget::Block => {
            let reason = reason.ok_or(GoalOperationError::ReasonRequired)?;
            let request_id = request_id.ok_or(GoalOperationError::Denied)?;
            blocked = Some((reason.to_owned(), request_id.to_owned()));
            Some(GoalAction::BlockedAudit(audit_observation(
                goal, reason, request_id,
            )))
        }
    };
    let post = match action {
        None => pre,
        Some(action) => pre
            .step(action)
            .ok_or(GoalOperationError::IllegalTransition {
                target: target_name(target),
                status,
            })?,
    };
    Ok(GoalSetPlan {
        goal: goal.clone(),
        objective: input
            .objective
            .as_deref()
            .map(str::trim)
            .map(ToOwned::to_owned),
        token_budget: input.token_budget,
        post,
        auto_resume: input.auto_resume,
        completion_evidence: (target == GoalTarget::Complete)
            .then(|| reason.map(ToOwned::to_owned)),
        blocked,
    })
}

/// Writes an admitted set, unless the Goal moved since it was observed.
pub async fn apply_set_in_txn(
    txn: &ConfigApplyTxn<'_>,
    plan: &GoalSetPlan,
) -> anyhow::Result<Option<GoalDocument>> {
    let observed = &plan.goal;
    let Some(current) = load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id)
        .await?
        .filter(|current| {
            same_goal_assignment(current, observed)
                && current.status == observed.status
                && current.continuation_sequence() == observed.continuation_sequence()
        })
    else {
        return Ok(None);
    };
    let post = plan.post;
    let objective = plan
        .objective
        .clone()
        .unwrap_or_else(|| current.objective.clone());
    let token_budget = plan.token_budget.unwrap_or(current.token_budget);
    if objective == current.objective
        && token_budget == current.token_budget
        && Some(post) == current.state()
        && plan
            .auto_resume
            .is_none_or(|enabled| current.auto_resume_at_reset == Some(enabled))
        && plan.completion_evidence.is_none()
        && plan.blocked.is_none()
    {
        return Ok(Some(current));
    }
    let now = Utc::now();
    let now_string = now.to_rfc3339();
    let mut input = serde_json::json!({
        "objective": objective,
        "status": post.status.as_str(),
        "token_budget": token_budget,
        "active_time_seconds": current.current_active_time_seconds(now),
        "active_started_at": post.status.accrues_active_time().then_some(now_string.as_str()),
        "consecutive_blocked_audits": post.blocked_audits,
        "wrapup_requested": post.wrapup_requested,
        "wrapup_completed": post.wrapup_completed,
        "updated_at": now_string,
    });
    if let Some(enabled) = plan.auto_resume {
        input["auto_resume_at_reset"] = enabled.into();
    }
    if let Some(evidence) = &plan.completion_evidence {
        input["completion_evidence"] = evidence.clone().into();
        input["last_failure"] = serde_json::Value::Null;
    }
    if let Some((reason, request_id)) = &plan.blocked {
        input["last_blocked_reason"] = reason.clone().into();
        input["last_blocked_request_id"] = request_id.clone().into();
    }
    let doc_id = gents_store::graphql::escape_graphql_string(&current.doc_id);
    let node_did = gents_store::graphql::escape_graphql_string(&current.node_did);
    let status = gents_store::graphql::escape_graphql_string(&current.status);
    let sequence = current.continuation_sequence();
    let response = txn
        .execute_client_signed_update(
            &format!(
                r#"mutation($input: GoalMutationInputArg!) {{
                update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }}, status: {{ _eq: "{status}" }}, continuation_sequence: {{ _eq: {sequence} }} }}, input: $input) {{ _docID }}
            }}"#
            ),
            &serde_json::json!({ "input": input }),
            gents_protocol::schemas::GOAL,
        )
        .await?;
    if !response["data"]["update_Goal"]
        .as_array()
        .is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"] == current.doc_id)
    {
        return Ok(None);
    }
    let updated = load_canonical_goal_in_txn(txn, &current.node_did, &current.session_id)
        .await?
        .context("updated Goal row disappeared")?;
    crate::triggers::engine::durable::publish_goal_outcomes(
        txn,
        &updated.node_did,
        &updated.goal_id,
        updated
            .last_blocked_reason
            .as_deref()
            .unwrap_or(&updated.status),
        &now_string,
    )
    .await?;
    Ok(Some(updated))
}

/// Sets the session's Goal as `actor` within `txn`.
pub async fn set_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: GoalActor,
    node_did: &str,
    session_id: &str,
    input: &GoalSetInput,
    obligations_unmet: Option<&str>,
    request_id: Option<&str>,
) -> anyhow::Result<Refusal<GoalDocument>> {
    let Some(goal) = load_canonical_goal_in_txn(txn, node_did, session_id).await? else {
        return Ok(Err(GoalOperationError::NoGoal(session_id.to_owned())));
    };
    let plan =
        match admit_set_in_txn(txn, actor, &goal, input, obligations_unmet, request_id).await? {
            Ok(plan) => plan,
            Err(refusal) => return Ok(Err(refusal)),
        };
    Ok(apply_set_in_txn(txn, &plan)
        .await?
        .ok_or(GoalOperationError::Stale))
}

/// Sets the session's Goal as whoever `access` acts for.
pub async fn set(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    input: &GoalSetInput,
) -> Result<GoalDocument, GoalOperationError> {
    access
        .transact_signed_conflict_rebuild("goal.set", move |txn| {
            Box::pin(async move {
                let Some(session) =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                else {
                    return Ok(Err(GoalOperationError::SessionMissing(
                        session_id.to_owned(),
                    )));
                };
                let actor = acting_in_txn(txn, node_did, &session.session.agent_did).await?;
                set_in_txn(txn, actor, node_did, session_id, input, None, None).await
            })
        })
        .await
        .map_err(passthrough)?
}

/// Sets the Goal the working Agent's calling request is assigned, as that
/// Agent.
pub async fn set_as_working_agent(
    access: &ConfigAccess,
    observed: &GoalDocument,
    input: &GoalSetInput,
    obligations_unmet: Option<&str>,
    request_id: &str,
) -> Result<GoalDocument, GoalOperationError> {
    let input = GoalSetInput {
        expected: Some(GoalExpected::Assignment {
            goal_doc_id: observed.doc_id.clone(),
            goal_id: observed.goal_id.clone(),
            assignment_root_request_doc_id: observed.assignment_root_request_doc_id.clone(),
            status: observed
                .parsed_status()
                .ok_or(GoalOperationError::IllegalTransition {
                    target: target_name(input.target()),
                    status: "unknown",
                })?,
            continuation_sequence: observed.continuation_sequence(),
        }),
        ..input.clone()
    };
    let input = &input;
    access
        .transact_write("goal.set_as_working_agent", move |txn| {
            Box::pin(async move {
                let Some(session) =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        &observed.node_did,
                        &observed.session_id,
                    )
                    .await?
                else {
                    return Ok(Err(GoalOperationError::SessionMissing(
                        observed.session_id.clone(),
                    )));
                };
                let actor =
                    acting_in_txn(txn, &observed.node_did, &session.session.agent_did).await?;
                set_in_txn(
                    txn,
                    actor,
                    &observed.node_did,
                    &observed.session_id,
                    input,
                    obligations_unmet,
                    Some(request_id),
                )
                .await
            })
        })
        .await
        .map_err(passthrough)?
}

fn passthrough<E>(error: E) -> GoalOperationError
where
    E: Into<anyhow::Error>,
{
    GoalOperationError::Passthrough(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

#[cfg(test)]
mod edit_cases_tests;

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn start_decision_matches_lean() {
        gents_lean_vocab::families::goal_automation_operations_start_decision::replay(|i| {
            start_decision(i.actor, i.session, i.existing, i.expected, i.claim)
        });
    }

    #[test]
    fn set_decision_matches_lean() {
        gents_lean_vocab::families::goal_automation_operations_set_decision::replay(|i| {
            set_decision(
                i.actor,
                i.status,
                i.target,
                i.fields,
                i.auto_resume,
                i.obligations_met,
            )
        });
    }
}
