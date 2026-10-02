//! Why a request stopped, read from its durable rows.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};
use gents_protocol::behavior_readiness::is_behavior_unavailable_rejection;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::{ConfigReferences, InferenceBackend};
use crate::graphql::escape_graphql_string;
use crate::oauth_credential::{
    account_failure_state, backend_account, list_accounts, serving_account, AccountState,
    AccountSummary, ServingAccount,
};
use crate::request_admission::SIGNED_REQUEST_FIELDS;
use crate::Collection;

/// The failed inference call that ended a request.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FailedCall {
    pub(crate) backend_id: Option<String>,
    pub(crate) behavior_id: Option<String>,
    pub(crate) call_kind: Option<String>,
    pub(crate) failure_reason: Option<String>,
    pub(crate) queued_at: Option<String>,
    pub(crate) started_at: Option<String>,
}

/// Why a turn stopped where the operator can act: a usage limit, or an
/// account that cannot serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    UsageLimit,
    AccountDisabled,
    AccountRemoved,
    AccountSignedOut,
}

/// An account as a blocked turn names it: a label, never an identity or token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedAccount {
    pub label: String,
    /// The sign-in provider, else the backend's provider kind.
    pub provider: String,
}

/// A stopped turn: the reason, the account and profile it ran on, the
/// behaviors that profile serves, the reported reset and the command that
/// moves the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedTurn {
    pub reason: BlockedReason,
    pub account: Option<BlockedAccount>,
    pub profile: Option<String>,
    pub behaviors_on_profile: Vec<String>,
    /// `None` when the provider reported no reset.
    pub resets_at: Option<DateTime<Utc>>,
    pub switch_command: Option<String>,
}

/// The blocked value of a request, from one configuration snapshot, the
/// principal's accounts, the request row and its last failed call. A call
/// that started before its account's sign-in connected names no account.
pub(crate) fn blocked_turn_from(
    references: &ConfigReferences,
    accounts: &[AccountSummary],
    request: &AgentRequestRow,
    call: Option<&FailedCall>,
    now: DateTime<Utc>,
) -> Option<BlockedTurn> {
    if !matches!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Failed | RequestLifecycleState::Dead)
    ) {
        return None;
    }
    let behavior_id = call
        .and_then(|call| call.behavior_id.as_deref())
        .or(request.behavior_id.as_deref())
        .unwrap_or_default();
    let profiles = references.behavior_profiles(behavior_id);
    let stopped_by_call = call.and_then(|call| {
        let text = call.failure_reason.as_deref()?;
        let (reason, resets_at) = match classify_provider_limit(text, now) {
            Some(ProviderLimit::UsageExhausted(limit)) => {
                (BlockedReason::UsageLimit, limit.resets_at)
            }
            _ => match account_failure_state(text)? {
                "disabled" => (BlockedReason::AccountDisabled, None),
                "removed from this node" => (BlockedReason::AccountRemoved, None),
                "signed out (expired or revoked)" => (BlockedReason::AccountSignedOut, None),
                _ => return None,
            },
        };
        let backend_id = call.backend_id.as_deref();
        let mut on_backend = profiles.iter().filter(|profile| {
            references
                .profile_with_backend(profile)
                .ok()
                .flatten()
                .is_some_and(|(profile, _)| Some(profile.backend_id.as_str()) == backend_id)
        });
        let profile = if call.call_kind.as_deref() == Some("compaction") {
            on_backend.next_back()
        } else {
            on_backend.next()
        };
        let started_at = call
            .started_at
            .as_deref()
            .or(call.queued_at.as_deref())
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok());
        let account = references
            .documents()
            .find(|((collection, id), _)| {
                *collection == Collection::InferenceBackend && Some(id.as_str()) == backend_id
            })
            .and_then(|(_, value)| serde_json::from_value::<InferenceBackend>(value.clone()).ok())
            .filter(|backend| {
                let since = backend_account(backend, accounts)
                    .flatten()
                    .and_then(|account| account.connected_at);
                started_at.is_some_and(|at| since.is_none_or(|since| at >= since))
            })
            .map(|backend| named(&backend, serving_account(&backend, accounts)));
        Some((reason, account, profile.cloned(), resets_at))
    });
    let (reason, account, profile, resets_at) = match stopped_by_call {
        Some(stopped) => stopped,
        None if request
            .failure_reason
            .as_deref()
            .is_some_and(is_behavior_unavailable_rejection) =>
        {
            profiles.iter().find_map(|profile| {
                let (_, backend) = references.profile_with_backend(profile).ok()??;
                let serving = serving_account(&backend, accounts);
                let reason = match serving.state {
                    AccountState::Enabled => return None,
                    AccountState::Disabled => BlockedReason::AccountDisabled,
                    AccountState::Missing => BlockedReason::AccountRemoved,
                };
                Some((
                    reason,
                    Some(named(&backend, serving)),
                    Some(profile.clone()),
                    None,
                ))
            })?
        }
        None => return None,
    };
    Some(BlockedTurn {
        reason,
        account,
        behaviors_on_profile: profile
            .as_deref()
            .map(|profile| references.behaviors_on_profile(profile))
            .unwrap_or_default(),
        switch_command: profile
            .as_deref()
            .map(|profile| format!("gents config profile set-account {profile}")),
        profile,
        resets_at,
    })
}

fn named(backend: &InferenceBackend, serving: ServingAccount) -> BlockedAccount {
    BlockedAccount {
        label: serving.label,
        provider: serving
            .provider
            .unwrap_or(backend.provider_kind.as_str())
            .to_owned(),
    }
}

/// Why `request_id` stopped, if it stopped where the operator can act.
/// An unknown request is an error.
pub async fn blocked_turn(
    access: &ConfigAccess,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<BlockedTurn>> {
    let (accounts, references, request, call) =
        stopped_request(access, agent_did, request_id).await?;
    Ok(blocked_turn_from(
        &references,
        &accounts,
        &request,
        call.as_ref(),
        Utc::now(),
    ))
}

/// What [`blocked_turn_from`] reads for `request_id`: the principal's
/// accounts, then one configuration snapshot, the request row and its last
/// failed call. An unknown request is an error.
pub(crate) async fn stopped_request(
    access: &ConfigAccess,
    agent_did: &str,
    request_id: &str,
) -> Result<(
    Vec<AccountSummary>,
    ConfigReferences,
    AgentRequestRow,
    Option<FailedCall>,
)> {
    let accounts = list_accounts(access, agent_did).await?;
    let (references, request, call) = access
        .transact("blocked_turn.request", |txn| {
            Box::pin(async move {
                let filter = format!(
                    r#"request_id: {{ _eq: "{}" }}"#,
                    escape_graphql_string(request_id)
                );
                let request = requests_in_txn(txn, agent_did, &filter)
                    .await?
                    .into_iter()
                    .next()
                    .with_context(|| format!("request {request_id:?} not found"))?;
                stopped_in_txn(txn, agent_did, request).await
            })
        })
        .await?;
    Ok((accounts, references, request, call))
}

/// [`blocked_turn`] for the latest request of `session_id`'s canonical Goal.
pub async fn blocked_goal_turn(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
) -> Result<Option<BlockedTurn>> {
    let accounts = list_accounts(access, agent_did).await?;
    let stopped = access
        .transact("blocked_turn.goal", |txn| {
            Box::pin(async move {
                let Some(goal) =
                    crate::goal::load_canonical_goal_in_txn(txn, agent_did, session_id).await?
                else {
                    return Ok(None);
                };
                let filter = format!(
                    r#"session_id: {{ _eq: "{}" }}"#,
                    escape_graphql_string(session_id)
                );
                let requests = requests_in_txn(txn, agent_did, &filter).await?;
                let Some(request) = crate::goal::latest_goal_request(&goal, &requests).cloned()
                else {
                    return Ok(None);
                };
                stopped_in_txn(txn, agent_did, request).await.map(Some)
            })
        })
        .await?;
    Ok(stopped.and_then(|(references, request, call)| {
        blocked_turn_from(&references, &accounts, &request, call.as_ref(), Utc::now())
    }))
}

/// `agent_did`'s requests matching `filter`, newest first.
async fn requests_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    filter: &str,
) -> Result<Vec<AgentRequestRow>> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(
                filter: {{ agent_did: {{ _eq: "{}" }}, {filter} }},
                order: [{{ created_at: DESC }}, {{ request_id: DESC }}]
            ) {{ {SIGNED_REQUEST_FIELDS} failure_reason }} }}"#,
            escape_graphql_string(agent_did)
        ))
        .await?;
    serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("AgentRequest query omitted rows")?,
    )
    .context("decoding AgentRequest rows")
}

async fn stopped_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    request: AgentRequestRow,
) -> Result<(ConfigReferences, AgentRequestRow, Option<FailedCall>)> {
    let call = last_failed_call_in_txn(txn, &request.request_id).await?;
    let references = ConfigReferences::load_in_txn(txn, agent_did).await?;
    Ok((references, request, call))
}

/// The request's latest failed call by `ended_at`. A call retried with a
/// higher `attempt` can end before a later call of another kind (compaction)
/// that actually stopped the request.
pub(crate) async fn last_failed_call_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<FailedCall>> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _eq: "{request_id}" }}, call_state: {{ _eq: "failed" }} }},
                order: [{{ ended_at: DESC }}, {{ attempt: DESC }}],
                limit: 1
            ) {{ backend_id behavior_id call_kind failure_reason queued_at started_at }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .context("InferenceCall query omitted rows")?;
    rows.first()
        .map(|row| serde_json::from_value(row.clone()).context("decode failed InferenceCall"))
        .transpose()
}

#[cfg(test)]
#[path = "blocked_turn/tests.rs"]
mod tests;
