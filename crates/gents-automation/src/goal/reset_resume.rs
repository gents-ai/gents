use super::*;
use crate::goal::blocked_turn::{goal_stopped_in_txn, served_profile};
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::identity::NodeIdentity;

#[derive(Debug)]
pub enum ResetResume {
    NotUsageLimited,
    Deferred,
    Resumed(GoalResumeReceipt),
}

pub async fn resume_at_reset(
    node: &std::sync::Arc<EmbeddedNode>,
    runtime: &dyn NodeIdentity,
    identity: &dyn NodeIdentity,
    goal: &GoalDocument,
    now: DateTime<Utc>,
) -> Result<ResetResume> {
    if goal.parsed_status() != Some(GoalStatus::UsageLimited) {
        return Ok(ResetResume::NotUsageLimited);
    }
    if goal.auto_resume_at_reset != Some(true)
        || reported_reset(goal.last_failure.as_deref(), now).is_none_or(|reset| reset > now)
    {
        return Ok(ResetResume::Deferred);
    }
    let (node_did, session_id) = (goal.node_did.as_str(), goal.session_id.as_str());
    let access = ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        identity::Did::new(identity.did())?,
        identity.managed_agent_signer(),
    )?;
    access
        .transact_write("goal.resume_at_reset", move |txn| {
            Box::pin(stage_reset_resume(
                txn, runtime, identity, node_did, session_id, now,
            ))
        })
        .await
}

async fn stage_reset_resume(
    txn: &ConfigApplyTxn<'_>,
    runtime: &dyn NodeIdentity,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<ResetResume> {
    let Some((goal, references, request, Some(call))) =
        goal_stopped_in_txn(txn, node_did, session_id).await?
    else {
        return Ok(ResetResume::Deferred);
    };
    anyhow::ensure!(
        txn.actor_did() == Some(identity.did()),
        "reset resume actor must be its signing agent"
    );
    if request.agent_did.as_deref() != Some(identity.did())
        || call.agent_did.as_deref() != Some(identity.did())
    {
        return Ok(ResetResume::Deferred);
    }
    let sessions = {
        use gents_protocol::document_query::{
            fields::agent_session as session_fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::new().field(session_fields::AGENT_DID))
                .filter(
                    Filter::eq(session_fields::NODE_DID, node_did)
                        .and(Filter::eq(session_fields::SESSION_ID, session_id)),
                )
                .limit(2)
                .render(),
        )
        .await?
    };
    let sessions = sessions
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array)
        .context("reset resume session query omitted rows")?;
    if sessions.len() != 1 || sessions[0]["agent_did"].as_str() != Some(identity.did()) {
        return Ok(ResetResume::Deferred);
    }
    if goal.parsed_status() != Some(GoalStatus::UsageLimited) {
        return Ok(ResetResume::NotUsageLimited);
    }
    if goal.auto_resume_at_reset != Some(true) {
        return Ok(ResetResume::Deferred);
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
        return Ok(ResetResume::Deferred);
    };
    if !(started < reset && reset <= now) {
        return Ok(ResetResume::Deferred);
    }
    let agent_did = identity.did();
    let Some(profile) = served_profile(&references, agent_did, &call) else {
        return Ok(ResetResume::Deferred);
    };
    let Some((_, backend)) = references.profile_with_backend(&profile)? else {
        return Ok(ResetResume::Deferred);
    };
    let account_serves = backend.enabled
        && match gents_inference::auth::credential::account_pick(&backend.auth) {
            Some(pick) => {
                use gents_inference::backends::provider::BackendProviderOauthExt;
                let Some(provider) = backend.provider_kind.oauth_provider() else {
                    return Ok(ResetResume::Deferred);
                };
                gents_inference::auth::credential::runtime_oauth_account_served_before(
                    txn, runtime, node_did, provider, pick, started,
                )
                .await?
            }
            None => backend.enabled,
        };
    if !account_serves {
        return Ok(ResetResume::Deferred);
    }
    super::operator_resume::stage_resume(txn, identity, node_did, session_id, &request.request_id)
        .await
        .map(ResetResume::Resumed)
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
