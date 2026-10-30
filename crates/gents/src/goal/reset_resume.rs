use super::*;
use crate::blocked_turn::{goal_stopped_in_txn, served_profile};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::identity::NodeIdentity;
use crate::oauth_credential::resolve_oauth_credential_in_txn;
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};

pub async fn resume_at_reset(
    node: &std::sync::Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    goal: &GoalDocument,
    now: DateTime<Utc>,
) -> Result<Option<GoalResumeReceipt>> {
    if goal.auto_resume_at_reset != Some(true)
        || reported_reset(goal.last_failure.as_deref(), now).is_none_or(|reset| reset > now)
    {
        return Ok(None);
    }
    let (node_did, session_id) = (goal.node_did.as_str(), goal.session_id.as_str());
    ConfigAccess::transact_local_write(node, None, "goal.resume_at_reset", move |txn| {
        Box::pin(stage_reset_resume(txn, identity, node_did, session_id, now))
    })
    .await
}

async fn stage_reset_resume(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<GoalResumeReceipt>> {
    let Some((goal, references, request, Some(call))) =
        goal_stopped_in_txn(txn, node_did, session_id).await?
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
    let agent_id = call
        .agent_id
        .as_deref()
        .or(request.agent_id.as_deref())
        .unwrap_or_default();
    let Some(profile) = served_profile(&references, agent_id, &call) else {
        return Ok(None);
    };
    let Some((_, backend)) = references.profile_with_backend(&profile)? else {
        return Ok(None);
    };
    let account_serves = backend.enabled
        && match backend.auth.account_pick() {
            Some(pick) => {
                use crate::backend_provider::BackendProviderOauthExt;
                let Some(provider) = backend.provider_kind.oauth_provider() else {
                    return Ok(None);
                };
                resolve_oauth_credential_in_txn(txn, node_did, provider, pick)
                    .await?
                    .is_some_and(|row| {
                        row.enabled && row.connected_at.is_none_or(|since| since <= started)
                    })
            }
            None => backend.enabled,
        };
    if !account_serves {
        return Ok(None);
    }
    super::operator_resume::stage_resume(txn, identity, node_did, session_id, &request.request_id)
        .await
        .map(Some)
}

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
