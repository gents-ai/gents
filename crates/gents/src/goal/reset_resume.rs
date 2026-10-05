//! A usage-limited Goal the operator opted in resumes by itself once the
//! reset its provider reported has passed (Lean `resumeBy`, `Cause.resetReached`).
use super::*;
use crate::blocked_turn::{goal_stopped_in_txn, served_profile};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::BackendAuth;
use crate::identity::AgentIdentity;
use crate::oauth_credential::{resolve_oauth_credential_in_txn, AccountPick};
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};

/// Resume `goal` through the operator resume transaction when its reported
/// reset is due at `now`; `None` when it is not. The transaction runs as the
/// node and `identity` signs the child, as claimed publication does. The
/// Goal's recorded limit gates the transaction, so nothing is read before
/// that reset passes.
pub async fn resume_at_reset(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    goal: &GoalDocument,
    now: DateTime<Utc>,
) -> Result<Option<GoalResumeReceipt>> {
    if goal.auto_resume_at_reset != Some(true)
        || reported_reset(goal.last_failure.as_deref(), now).is_none_or(|reset| reset > now)
    {
        return Ok(None);
    }
    let (agent_did, session_id) = (goal.agent_did.as_str(), goal.session_id.as_str());
    ConfigAccess::transact_local(node, None, "goal.resume_at_reset", move |txn| {
        Box::pin(stage_reset_resume(
            txn, identity, agent_did, session_id, now,
        ))
    })
    .await
}

/// `ResetFacts.due`, read in the resume transaction, then `resume`.
async fn stage_reset_resume(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn AgentIdentity,
    agent_did: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<GoalResumeReceipt>> {
    let Some((goal, references, request, Some(call))) =
        goal_stopped_in_txn(txn, agent_did, session_id).await?
    else {
        return Ok(None);
    };
    if goal.parsed_status() != Some(GoalStatus::UsageLimited)
        || goal.auto_resume_at_reset != Some(true)
    {
        return Ok(None);
    }
    let started = call
        .started_at
        .as_deref()
        .or(call.queued_at.as_deref())
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc));
    let (Some(reset), Some(started)) =
        (reported_reset(call.failure_reason.as_deref(), now), started)
    else {
        return Ok(None);
    };
    if !(started < reset && reset <= now) {
        return Ok(None);
    }
    let behavior_id = call
        .behavior_id
        .as_deref()
        .or(request.behavior_id.as_deref())
        .unwrap_or_default();
    let Some(profile) = served_profile(&references, behavior_id, &call) else {
        return Ok(None);
    };
    let Some((_, backend)) = references.profile_with_backend(&profile)? else {
        return Ok(None);
    };
    // The same sign-in: the account the backend names, enabled, connected
    // before the limited call.
    let account_serves = backend.enabled
        && match &backend.auth {
            BackendAuth::PrincipalOAuth { account_ref } => {
                use crate::backend_provider::BackendProviderOauthExt;
                let Some(provider) = backend.provider_kind.oauth_provider() else {
                    return Ok(None);
                };
                resolve_oauth_credential_in_txn(
                    txn,
                    agent_did,
                    provider,
                    AccountPick::Reference(account_ref.as_deref()),
                )
                .await?
                .is_some_and(|row| {
                    row.enabled && row.connected_at.is_none_or(|since| since <= started)
                })
            }
            _ => backend.enabled,
        };
    if !account_serves {
        return Ok(None);
    }
    super::operator_resume::stage_resume(txn, identity, agent_did, session_id, &request.request_id)
        .await
        .map(Some)
}

/// The reset a recorded usage limit names, if it names one.
fn reported_reset(text: Option<&str>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match classify_provider_limit(text?, now)? {
        ProviderLimit::UsageExhausted(limit) => limit.resets_at,
        _ => None,
    }
}

#[cfg(test)]
use super::operator_resume::support;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod tests;
