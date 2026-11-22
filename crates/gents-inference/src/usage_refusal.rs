//! Provider usage refusals that still stand, read from the calls the reader
//! may read (Lean `Identity.ProviderUsageEvidence`): an Agent's views read as
//! that Agent, the operator's as the User.

use chrono::{DateTime, Utc};
use gents_documents::InferenceProfile;
use gents_protocol::provider_limit::{ProviderLimitStatement, ProviderLimitVerdict};
use gents_store::config_client::ConfigRead;
use gents_store::views::{read_views, ViewError};
use serde::{Deserialize, Serialize};

use crate::views::usage_refusals::{LatestCall, LatestCallKind, LatestCallRow};

/// A provider usage limit that stopped a call on one backend and model and
/// still stands (Lean `InferenceCall.UsageRefusal.stands`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRefusal {
    pub verdict: ProviderLimitVerdict,
    pub backend_id: String,
    pub model: String,
    pub profiles: Vec<String>,
    pub resets_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub statement: ProviderLimitStatement,
    pub refused_at: DateTime<Utc>,
    pub agent_did: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum UsageRefusalError {
    #[error("reading provider usage refusals: {0}")]
    Read(#[from] ViewError),
    #[error("InferenceCall {call_id} records {field} {value:?}, which is not an RFC 3339 time")]
    Time {
        call_id: String,
        field: &'static str,
        value: String,
    },
}

/// Lean `InferenceCall.UsageRefusal.stands`: a refusal stands until its
/// reported reset passes or a call on its backend and model queued strictly
/// after it completes.
pub fn stands(
    now: DateTime<Utc>,
    refused_at: DateTime<Utc>,
    resets_at: Option<DateTime<Utc>>,
    last_completion: Option<DateTime<Utc>>,
) -> bool {
    resets_at.is_none_or(|reset| now < reset)
        && last_completion.is_none_or(|completed| completed <= refused_at)
}

/// The standing refusals on the backends and models `profiles` select, read
/// as `reader` sees the calls.
pub async fn standing_refusals<R>(
    reader: &R,
    node_did: &str,
    profiles: &[InferenceProfile],
    now: DateTime<Utc>,
) -> Result<Vec<UsageRefusal>, UsageRefusalError>
where
    R: ConfigRead + ?Sized,
{
    let mut keys: Vec<(&str, &str)> = profiles
        .iter()
        .map(|profile| (profile.backend_id.as_str(), profile.model_name.as_str()))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let views: Vec<LatestCall> = keys
        .iter()
        .flat_map(|(backend_id, model_name)| {
            [LatestCallKind::UsageLimited, LatestCallKind::Completed].map(|kind| LatestCall {
                node_did: node_did.to_owned(),
                backend_id: (*backend_id).to_owned(),
                model_name: (*model_name).to_owned(),
                kind,
            })
        })
        .collect();
    let rows = read_views(reader, &views).await?;
    let mut refusals = Vec::new();
    for ((backend_id, model), latest) in keys.iter().zip(rows.chunks(2)) {
        let [refused, completed] = latest else {
            continue;
        };
        let Some(refused) = refused.first() else {
            continue;
        };
        let refused_at = queued_at(refused)?;
        let resets_at = refused
            .provider_limit_resets_at
            .as_deref()
            .map(|value| time(refused, "provider_limit_resets_at", value))
            .transpose()?;
        let last_completion = completed.first().map(queued_at).transpose()?;
        if !stands(now, refused_at, resets_at, last_completion) {
            continue;
        }
        refusals.push(UsageRefusal {
            verdict: ProviderLimitVerdict::UsageLimited,
            backend_id: (*backend_id).to_owned(),
            model: (*model).to_owned(),
            profiles: profiles
                .iter()
                .filter(|profile| profile.backend_id == *backend_id && profile.model_name == *model)
                .map(|profile| profile.profile_id.clone())
                .collect(),
            resets_at,
            statement: ProviderLimitStatement {
                window_minutes: refused.provider_limit_window_minutes,
                tokens_used: refused.provider_limit_tokens_used,
                tokens_limit: refused.provider_limit_tokens_limit,
            },
            refused_at,
            agent_did: refused.agent_did.clone(),
        });
    }
    Ok(refusals)
}

fn queued_at(row: &LatestCallRow) -> Result<DateTime<Utc>, UsageRefusalError> {
    time(
        row,
        "queued_at",
        row.queued_at.as_deref().unwrap_or_default(),
    )
}

fn time(
    row: &LatestCallRow,
    field: &'static str,
    value: &str,
) -> Result<DateTime<Utc>, UsageRefusalError> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| UsageRefusalError::Time {
            call_id: row.call_id.clone(),
            field,
            value: value.to_owned(),
        })
}

/// A refusal as an Agent reads it: the refusal, then the call that moves the
/// refused profile to a backend and model that can serve it.
#[derive(Debug, Serialize)]
pub struct AgentUsageRefusal<'a> {
    #[serde(flatten)]
    pub refusal: &'a UsageRefusal,
    pub next_call: serde_json::Value,
}

impl UsageRefusal {
    pub fn for_agent(&self) -> AgentUsageRefusal<'_> {
        let mut args = serde_json::json!({
            "argv": ["profile", "preview", "edit"],
            "set": {"backend_id": "<BACKEND_ID>", "model_name": "<MODEL>"},
        });
        if let Some(agent) = &self.agent_did {
            args["options"] = serde_json::json!({ "agent": agent });
        }
        AgentUsageRefusal {
            refusal: self,
            next_call: serde_json::json!({"tool": "config", "args": args}),
        }
    }
}

#[cfg(test)]
#[path = "usage_refusal/tests.rs"]
mod tests;
