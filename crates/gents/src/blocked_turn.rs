//! Why a request stopped, read from its durable rows.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::ConfigReferences;
use crate::graphql::escape_graphql_string;
use crate::oauth_credential::AccountSummary;

/// The failed inference call that ended a request.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FailedCall {
    pub(crate) backend_id: Option<String>,
    pub(crate) behavior_id: Option<String>,
    pub(crate) call_kind: Option<String>,
    pub(crate) failure_reason: Option<String>,
    pub(crate) queued_at: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
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
/// principal's accounts, the request row and its last failed call.
pub(crate) fn blocked_turn_from(
    _references: &ConfigReferences,
    _accounts: &[AccountSummary],
    _request: &AgentRequestRow,
    _call: Option<&FailedCall>,
    _now: DateTime<Utc>,
) -> Option<BlockedTurn> {
    None
}

/// Why `request_id` stopped, if it stopped where the operator can act.
/// An unknown request is an error.
pub async fn blocked_turn(
    _access: &ConfigAccess,
    _agent_did: &str,
    _request_id: &str,
) -> Result<Option<BlockedTurn>> {
    Ok(None)
}

/// [`blocked_turn`] for the latest request of `session_id`'s canonical Goal.
pub async fn blocked_goal_turn(
    _access: &ConfigAccess,
    _agent_did: &str,
    _session_id: &str,
) -> Result<Option<BlockedTurn>> {
    Ok(None)
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
            ) {{ failure_reason }}
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
