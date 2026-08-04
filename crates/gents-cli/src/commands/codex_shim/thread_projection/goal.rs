use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_codex_protocol as codex;

use gents::goal::{
    delete_goals_for_session, load_canonical_goal, refresh_goal_usage, set_goal, GoalDocument,
    GoalSnapshot, GoalStatus,
};

use crate::commands::codex_shim::ShimState;

pub(in crate::commands::codex_shim) async fn set_codex_thread_goal(
    state: &ShimState,
    params: &codex::ThreadGoalSetParams,
) -> Result<Option<codex::ThreadGoal>> {
    if super::storage::load_scoped_session(state, &params.thread_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let status = params.status.map(codex_status_to_goal_status);
    let goal = set_goal(
        state.node.as_ref(),
        state.agent_did.as_ref(),
        &params.thread_id,
        params.objective.as_deref(),
        status,
        params.token_budget,
    )
    .await?;
    Ok(Some(enrich(state, goal).await?))
}

pub(in crate::commands::codex_shim) async fn get_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<codex::ThreadGoal>> {
    if super::storage::load_scoped_session(state, thread_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let Some(goal) =
        load_canonical_goal(state.node.as_ref(), state.agent_did.as_ref(), thread_id).await?
    else {
        return Ok(None);
    };
    Ok(Some(enrich(state, goal).await?))
}

pub(in crate::commands::codex_shim) async fn clear_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<bool> {
    if super::storage::load_scoped_session(state, thread_id)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    Ok(
        delete_goals_for_session(state.node.as_ref(), state.agent_did.as_ref(), thread_id).await?
            > 0,
    )
}

async fn enrich(state: &ShimState, mut goal: GoalDocument) -> Result<codex::ThreadGoal> {
    refresh_goal_usage(state.node.as_ref(), &goal).await?;
    goal = load_canonical_goal(
        state.node.as_ref(),
        state.agent_did.as_ref(),
        &goal.session_id,
    )
    .await?
    .unwrap_or(goal);
    let snapshot = GoalSnapshot::from_document(&goal, Utc::now());
    Ok(codex::ThreadGoal {
        thread_id: snapshot.session_id,
        objective: snapshot.objective,
        status: goal_status_to_codex(&snapshot.status)?,
        token_budget: snapshot.token_budget,
        tokens_used: snapshot.tokens_used,
        time_used_seconds: snapshot.active_time_seconds,
        created_at: epoch_seconds(snapshot.created_at.as_deref()),
        updated_at: epoch_seconds(snapshot.updated_at.as_deref()),
    })
}

fn codex_status_to_goal_status(status: codex::ThreadGoalStatus) -> GoalStatus {
    match status {
        codex::ThreadGoalStatus::Active => GoalStatus::Active,
        codex::ThreadGoalStatus::Paused => GoalStatus::Paused,
        codex::ThreadGoalStatus::Blocked => GoalStatus::Blocked,
        codex::ThreadGoalStatus::UsageLimited => GoalStatus::UsageLimited,
        codex::ThreadGoalStatus::BudgetLimited => GoalStatus::BudgetLimited,
        codex::ThreadGoalStatus::Complete => GoalStatus::Complete,
    }
}

fn goal_status_to_codex(status: &str) -> Result<codex::ThreadGoalStatus> {
    Ok(
        match GoalStatus::parse(status).context("invalid durable Goal status")? {
            GoalStatus::Active => codex::ThreadGoalStatus::Active,
            GoalStatus::Paused => codex::ThreadGoalStatus::Paused,
            GoalStatus::Blocked => codex::ThreadGoalStatus::Blocked,
            GoalStatus::UsageLimited => codex::ThreadGoalStatus::UsageLimited,
            GoalStatus::BudgetLimited => codex::ThreadGoalStatus::BudgetLimited,
            GoalStatus::Complete => codex::ThreadGoalStatus::Complete,
        },
    )
}

fn epoch_seconds(value: Option<&str>) -> i64 {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp())
        .unwrap_or_default()
}
