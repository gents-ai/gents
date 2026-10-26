use chrono::{DateTime, Utc};
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};
use gents_protocol::node_readiness::is_behavior_unavailable_rejection;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use crate::document_config::{ConfigReferences, InferenceBackend};
use crate::graphql::escape_graphql_string;
use crate::oauth_credential::{
    account_failure_state, backend_account, list_accounts, serving_account, AccountState,
    AccountSummary, ServingAccount,
};
use crate::request_admission::SIGNED_REQUEST_FIELDS;
use crate::Collection;

#[derive(Debug, thiserror::Error)]
pub enum BlockedTurnError {
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error("request {request_id:?} not found")]
    RequestNotFound { request_id: String },
    #[error("AgentRequest query omitted rows")]
    RequestRowsOmitted,
    #[error("decoding AgentRequest rows")]
    DecodeRequestRows(#[source] serde_json::Error),
    #[error("InferenceCall query omitted rows")]
    InferenceCallRowsOmitted,
    #[error("decode failed InferenceCall")]
    DecodeFailedCall(#[source] serde_json::Error),
    #[error(transparent)]
    Goal(Box<crate::goal::GoalError>),
    #[error(transparent)]
    OAuthCredential(#[from] crate::oauth_credential::OAuthCredentialError),
    #[error(transparent)]
    DocumentConfig(Box<crate::document_config::DocumentConfigError>),
}

impl From<crate::document_config::DocumentConfigError> for BlockedTurnError {
    fn from(error: crate::document_config::DocumentConfigError) -> Self {
        Self::DocumentConfig(Box::new(error))
    }
}

impl From<crate::goal::GoalError> for BlockedTurnError {
    fn from(error: crate::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}

type Result<T, E = BlockedTurnError> = std::result::Result<T, E>;

impl crate::config_client::TransactionFailure for BlockedTurnError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FailedCall {
    pub(crate) backend_id: Option<String>,
    pub(crate) agent_id: Option<String>,
    pub(crate) call_kind: Option<String>,
    pub(crate) failure_reason: Option<String>,
    pub(crate) queued_at: Option<String>,
    pub(crate) started_at: Option<String>,
}

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
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedTurn {
    pub reason: BlockedReason,
    pub account: Option<BlockedAccount>,
    pub profile: Option<String>,
    pub agents_on_profile: Vec<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub switch_command: Option<String>,
}

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
    let agent_id = call
        .and_then(|call| call.agent_id.as_deref())
        .or(request.agent_id.as_deref())
        .unwrap_or_default();
    let profiles = references.agent_profiles(agent_id);
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
        let profile = served_profile(references, agent_id, call);
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
        Some((reason, account, profile, resets_at))
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
        agents_on_profile: profile
            .as_deref()
            .map(|profile| references.agents_on_profile(profile))
            .unwrap_or_default(),
        switch_command: profile
            .as_deref()
            .map(|profile| format!("gents config profile set-account {profile}")),
        profile,
        resets_at,
    })
}

pub(crate) fn served_profile(
    references: &ConfigReferences,
    agent_id: &str,
    call: &FailedCall,
) -> Option<String> {
    let profiles = references.agent_profiles(agent_id);
    let served = if call.call_kind.as_deref() == Some("compaction") {
        profiles.last()
    } else {
        profiles.first()
    };
    served
        .filter(|profile| {
            references
                .profile_with_backend(profile)
                .ok()
                .flatten()
                .is_some_and(|(profile, _)| Some(&profile.backend_id) == call.backend_id.as_ref())
        })
        .cloned()
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

pub async fn blocked_turn(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
) -> Result<Option<BlockedTurn>> {
    let (accounts, references, request, call) =
        stopped_request(access, node_did, request_id).await?;
    Ok(blocked_turn_from(
        &references,
        &accounts,
        &request,
        call.as_ref(),
        Utc::now(),
    ))
}

pub(crate) async fn stopped_request(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
) -> Result<(
    Vec<AccountSummary>,
    ConfigReferences,
    AgentRequestRow,
    Option<FailedCall>,
)> {
    let accounts = list_accounts(access, node_did).await?;
    let (references, request, call) = access
        .transact_readonly("blocked_turn.request", |txn| {
            Box::pin(async move {
                let filter = format!(
                    r#"request_id: {{ _eq: "{}" }}"#,
                    escape_graphql_string(request_id)
                );
                let request = requests_in_txn(txn, node_did, &filter)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| BlockedTurnError::RequestNotFound {
                        request_id: request_id.to_owned(),
                    })?;
                stopped_in_txn(txn, node_did, request).await
            })
        })
        .await?;
    Ok((accounts, references, request, call))
}

pub async fn blocked_goal_turn(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> Result<Option<BlockedTurn>> {
    let accounts = list_accounts(access, node_did).await?;
    let stopped = access
        .transact_readonly("blocked_turn.goal", |txn| {
            Box::pin(goal_stopped_in_txn(txn, node_did, session_id))
        })
        .await?;
    Ok(stopped.and_then(|(_, references, request, call)| {
        blocked_turn_from(&references, &accounts, &request, call.as_ref(), Utc::now())
    }))
}

pub(crate) async fn goal_stopped_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<
    Option<(
        crate::goal::GoalDocument,
        ConfigReferences,
        AgentRequestRow,
        Option<FailedCall>,
    )>,
> {
    let Some(goal) = crate::goal::load_canonical_goal_in_txn(txn, node_did, session_id).await?
    else {
        return Ok(None);
    };
    let filter = format!(
        r#"session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(session_id)
    );
    let requests = requests_in_txn(txn, node_did, &filter).await?;
    let Some(request) = crate::goal::latest_goal_request(&goal, &requests).cloned() else {
        return Ok(None);
    };
    let (references, request, call) = stopped_in_txn(txn, node_did, request).await?;
    Ok(Some((goal, references, request, call)))
}

async fn requests_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    filter: &str,
) -> Result<Vec<AgentRequestRow>, BlockedTurnError> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(
                filter: {{ node_did: {{ _eq: "{}" }}, {filter} }},
                order: [{{ created_at: DESC }}, {{ request_id: DESC }}]
            ) {{ {SIGNED_REQUEST_FIELDS} failure_reason }} }}"#,
            escape_graphql_string(node_did)
        ))
        .await?;
    serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .ok_or(BlockedTurnError::RequestRowsOmitted)?,
    )
    .map_err(BlockedTurnError::DecodeRequestRows)
}

async fn stopped_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    request: AgentRequestRow,
) -> Result<(ConfigReferences, AgentRequestRow, Option<FailedCall>)> {
    let call = last_failed_call_in_txn(txn, &request.request_id).await?;
    let references = ConfigReferences::load_in_txn(txn, node_did).await?;
    Ok((references, request, call))
}

/// The request's latest failed call by `ended_at`; a retried call with a
/// higher `attempt` can end before a later compaction call that stopped it.
pub(crate) async fn last_failed_call_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<FailedCall>, BlockedTurnError> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _eq: "{request_id}" }}, call_state: {{ _eq: "failed" }} }},
                order: [{{ ended_at: DESC }}, {{ attempt: DESC }}],
                limit: 1
            ) {{ backend_id agent_id call_kind failure_reason queued_at started_at }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .ok_or(BlockedTurnError::InferenceCallRowsOmitted)?;
    rows.first()
        .map(|row| serde_json::from_value(row.clone()).map_err(BlockedTurnError::DecodeFailedCall))
        .transpose()
}

#[cfg(test)]
#[path = "blocked_turn/tests.rs"]
mod tests;
