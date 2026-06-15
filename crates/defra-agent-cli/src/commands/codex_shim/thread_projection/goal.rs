use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use codex_app_server_protocol as codex;
use serde::{Deserialize, Serialize};

use crate::commands::codex_shim::thread_projection::session_token_usage;
use crate::commands::codex_shim::ShimState;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredGoal {
    pub(in crate::commands::codex_shim) thread_id: String,
    pub(in crate::commands::codex_shim) objective: String,
    pub(in crate::commands::codex_shim) status: codex::ThreadGoalStatus,
    pub(in crate::commands::codex_shim) token_budget: Option<i64>,
    pub(in crate::commands::codex_shim) created_at: i64,
    pub(in crate::commands::codex_shim) updated_at: i64,
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_goal(
    state: &ShimState,
    params: &codex::ThreadGoalSetParams,
) -> Result<Option<codex::ThreadGoal>> {
    // Don't seed goal state for a thread this shim doesn't own; a later resume of
    // the same id would otherwise inherit a phantom goal.
    if super::storage::load_scoped_session(state, &params.thread_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let existing = state.thread_goal(&params.thread_id).await;
    let now = now_seconds_i64();
    let status = params
        .status
        .as_ref()
        .copied()
        .or_else(|| existing.as_ref().map(|goal| goal.status.clone()))
        .unwrap_or(codex::ThreadGoalStatus::Active);
    let token_budget = match &params.token_budget {
        Some(value) => *value,
        None => existing.as_ref().and_then(|goal| goal.token_budget),
    };
    let created_at = existing.as_ref().map(|goal| goal.created_at).unwrap_or(now);
    let goal = StoredGoal {
        thread_id: params.thread_id.clone(),
        objective: params
            .objective
            .clone()
            .or_else(|| existing.as_ref().map(|goal| goal.objective.clone()))
            .unwrap_or_default(),
        status,
        token_budget,
        created_at,
        updated_at: now,
    };
    state.set_thread_goal(&params.thread_id, goal.clone()).await;
    Ok(Some(enrich(state, goal).await?))
}

pub(in crate::commands::codex_shim) async fn get_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<codex::ThreadGoal>> {
    match state.thread_goal(thread_id).await {
        Some(goal) => Ok(Some(enrich(state, goal).await?)),
        None => Ok(None),
    }
}

pub(in crate::commands::codex_shim) async fn clear_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<bool> {
    Ok(state.clear_thread_goal(thread_id).await)
}

async fn enrich(state: &ShimState, goal: StoredGoal) -> Result<codex::ThreadGoal> {
    let tokens_used = session_token_usage(state, &goal.thread_id).await?.total();
    let time_used_seconds = (now_seconds_i64() - goal.created_at).max(0);
    Ok(codex::ThreadGoal {
        thread_id: goal.thread_id,
        objective: goal.objective,
        status: goal.status,
        token_budget: goal.token_budget,
        tokens_used,
        time_used_seconds,
        created_at: goal.created_at,
        updated_at: goal.updated_at,
    })
}

fn now_seconds_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
