use super::*;
use crate::goal::blocked_turn::{goal_stopped_in_txn, served_profile};
use crate::inference::auth::credential::resolve_oauth_credential_in_txn;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::identity::NodeIdentity;
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
    let access = ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        identity::Did::new(identity.did())?,
        identity.managed_agent_signer(),
    )?;
    access
        .transact_write("goal.resume_at_reset", move |txn| {
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
    anyhow::ensure!(
        txn.actor_did() == Some(identity.did()),
        "reset resume actor must be its signing agent"
    );
    if request.agent_did.as_deref() != Some(identity.did())
        || call.agent_did.as_deref() != Some(identity.did())
    {
        return Ok(None);
    }
    let sessions = txn.execute(&format!(r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did }} }}"#, escape_graphql_string(node_did), escape_graphql_string(session_id))).await?;
    let sessions = sessions
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array)
        .context("reset resume session query omitted rows")?;
    if sessions.len() != 1 || sessions[0]["agent_did"].as_str() != Some(identity.did()) {
        return Ok(None);
    }
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
    let agent_did = identity.did();
    let Some(profile) = served_profile(&references, agent_did, &call) else {
        return Ok(None);
    };
    let Some((_, backend)) = references.profile_with_backend(&profile)? else {
        return Ok(None);
    };
    let account_serves = backend.enabled
        && match backend.auth.account_pick() {
            Some(pick) => {
                use crate::inference::backends::provider::BackendProviderOauthExt;
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
