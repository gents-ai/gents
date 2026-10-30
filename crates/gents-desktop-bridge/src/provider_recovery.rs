use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRecoveryRequest {
    pub node_did: String,
    pub session_id: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecoveryCandidate {
    pub backend_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecoveryGoal {
    pub goal_id: String,
    pub status: String,
    pub auto_resume_at_reset: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecoveryPlan {
    pub reason: String,
    pub account_label: Option<String>,
    pub profile_id: Option<String>,
    pub affected_agents: Vec<String>,
    pub resets_at: Option<String>,
    pub candidates: Vec<ProviderRecoveryCandidate>,
    pub companion_profiles: Vec<String>,
    pub switch_cost: String,
    pub unavailable_reason: Option<String>,
    pub goal: Option<ProviderRecoveryGoal>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ProviderRecoveryAction {
    MoveProfile {
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(rename = "backendId")]
        backend_id: String,
        #[serde(rename = "withCompaction")]
        with_compaction: bool,
    },
    ResumeGoal {
        #[serde(rename = "goalId")]
        goal_id: String,
        #[serde(rename = "backendId")]
        backend_id: String,
        #[serde(rename = "withCompaction")]
        with_compaction: bool,
    },
    SetAutoResume {
        #[serde(rename = "goalId")]
        goal_id: String,
        enabled: bool,
    },
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRecoveryApply {
    pub scope: ProviderRecoveryRequest,
    pub action: ProviderRecoveryAction,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProviderRecoveryResult {
    Applied {
        message: String,
        #[serde(rename = "requestId")]
        request_id: Option<String>,
    },
    Stale {
        reason: String,
    },
    Refused {
        reason: String,
    },
}

async fn validate_scope(access: &ConfigAccess, request: &ProviderRecoveryRequest) -> Result<()> {
    use gents::graphql::escape_graphql_string;
    anyhow::ensure!(
        !request.node_did.trim().is_empty()
            && !request.session_id.trim().is_empty()
            && !request.request_id.trim().is_empty(),
        "node, session and stopped request are required"
    );
    let response = access.execute(&format!(
        "{{ AgentRequest(filter: {{ node_did: {{ _eq: \"{}\" }}, request_id: {{ _eq: \"{}\" }} }}, limit: 2) {{ session_id }} }}",
        escape_graphql_string(&request.node_did), escape_graphql_string(&request.request_id),
    )).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("stopped request observation is unavailable")?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0]["session_id"].as_str() == Some(&request.session_id),
        "stopped request no longer belongs to the selected session"
    );
    Ok(())
}

pub async fn plan(
    access: &ConfigAccess,
    request: &ProviderRecoveryRequest,
) -> Result<Option<ProviderRecoveryPlan>> {
    validate_scope(access, request).await?;
    let Some(blocked) =
        gents::goal::blocked_turn::blocked_turn(access, &request.node_did, &request.request_id)
            .await?
    else {
        return Ok(None);
    };
    let goal = access
        .transact_readonly("desktop.provider_recovery.goal", |txn| {
            Box::pin(gents::goal::load_canonical_goal_in_txn(
                txn,
                &request.node_did,
                &request.session_id,
            ))
        })
        .await?
        .map(|goal| ProviderRecoveryGoal {
            goal_id: goal.goal_id,
            status: goal.status,
            auto_resume_at_reset: goal.auto_resume_at_reset == Some(true),
        });
    let mut candidates = Vec::new();
    let mut companions = Vec::new();
    let mut unavailable = None;
    if let Some(profile) = blocked.profile.as_deref() {
        match gents::config_client::switch_candidates(
            access,
            &request.node_did,
            profile,
            &[],
            chrono::Utc::now(),
            &[],
        )
        .await
        {
            Ok(switch) => {
                companions = switch.companions;
                candidates = switch
                    .candidates
                    .into_iter()
                    .filter(|candidate| candidate.models == "offered")
                    .map(|candidate| ProviderRecoveryCandidate {
                        backend_id: candidate.backend_id,
                        label: candidate.label,
                    })
                    .collect();
                if candidates.is_empty() {
                    unavailable = Some(
                        "No other enabled account has reported support for this model.".into(),
                    );
                }
            }
            Err(error) => unavailable = Some(error.to_string()),
        }
    }
    Ok(Some(ProviderRecoveryPlan {
        reason: match blocked.reason {
            gents::goal::blocked_turn::BlockedReason::UsageLimit => "usage_limit",
            gents::goal::blocked_turn::BlockedReason::AccountDisabled => "account_disabled",
            gents::goal::blocked_turn::BlockedReason::AccountRemoved => "account_removed",
            gents::goal::blocked_turn::BlockedReason::AccountSignedOut => "account_signed_out",
        }
        .into(),
        account_label: blocked.account.map(|account| account.label),
        profile_id: blocked.profile,
        affected_agents: blocked.agents_on_profile,
        resets_at: blocked.resets_at.map(|time| time.to_rfc3339()),
        candidates,
        companion_profiles: companions,
        switch_cost: gents::config_client::SWITCH_COST.into(),
        unavailable_reason: unavailable,
        goal,
    }))
}

pub async fn apply(
    access: &ConfigAccess,
    identity: &dyn gents::NodeIdentity,
    request: &ProviderRecoveryApply,
) -> Result<ProviderRecoveryResult> {
    let Some(current) = plan(access, &request.scope).await? else {
        return Ok(ProviderRecoveryResult::Stale {
            reason: "This turn is no longer blocked on its account.".into(),
        });
    };
    let scope = &request.scope;
    let result = match &request.action {
        ProviderRecoveryAction::MoveProfile {
            profile_id,
            backend_id,
            with_compaction,
        } => {
            if current.profile_id.as_deref() != Some(profile_id.as_str()) {
                return Ok(ProviderRecoveryResult::Stale {
                    reason: "The stopped profile changed. Refresh before moving it.".into(),
                });
            }
            if !current
                .candidates
                .iter()
                .any(|candidate| candidate.backend_id == *backend_id)
            {
                return Ok(ProviderRecoveryResult::Refused {
                    reason: "That account is no longer eligible for this model.".into(),
                });
            }
            gents::config_client::switch_profile_account(
                access,
                &scope.node_did,
                profile_id,
                backend_id,
                *with_compaction,
                &[],
            )
            .await
            .map(|receipt| (receipt.headline, None))
        }
        ProviderRecoveryAction::ResumeGoal {
            goal_id,
            backend_id,
            with_compaction,
        } => {
            if !current
                .goal
                .as_ref()
                .is_some_and(|goal| goal.goal_id == *goal_id && goal.status == "usage_limited")
            {
                return Ok(ProviderRecoveryResult::Stale {
                    reason: "The Goal changed. Refresh before resuming it.".into(),
                });
            }
            if !current
                .candidates
                .iter()
                .any(|candidate| candidate.backend_id == *backend_id)
            {
                return Ok(ProviderRecoveryResult::Refused {
                    reason: "That account is no longer eligible for this model.".into(),
                });
            }
            gents::goal::resume_goal_on_account(
                access,
                identity,
                &scope.node_did,
                &scope.session_id,
                &scope.request_id,
                backend_id,
                *with_compaction,
                &|_| Ok(Vec::new()),
            )
            .await
            .map(|receipt| {
                (
                    "Resume requested on the selected account.".into(),
                    Some(receipt.resume.request_id),
                )
            })
        }
        ProviderRecoveryAction::SetAutoResume { goal_id, enabled } => {
            if !current
                .goal
                .as_ref()
                .is_some_and(|goal| goal.goal_id == *goal_id)
            {
                return Ok(ProviderRecoveryResult::Stale {
                    reason: "The Goal changed. Refresh its reset setting.".into(),
                });
            }
            gents::goal::set_goal_auto_resume_from_access(
                access,
                &scope.node_did,
                &scope.session_id,
                goal_id,
                *enabled,
            )
            .await
            .map(|_| ("Reset resume preference saved.".into(), None))
        }
    };
    Ok(match result {
        Ok((message, request_id)) => ProviderRecoveryResult::Applied {
            message,
            request_id,
        },
        Err(error) => ProviderRecoveryResult::Refused {
            reason: error.to_string(),
        },
    })
}

#[cfg(test)]
mod tests;
