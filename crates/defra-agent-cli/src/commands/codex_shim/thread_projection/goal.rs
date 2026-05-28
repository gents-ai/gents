use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::storage::load_projection;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredGoal {
    thread_id: String,
    objective: String,
    status: codex::ThreadGoalStatus,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
    created_at: i64,
    updated_at: i64,
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_goal(
    state: &ShimState,
    params: &codex::ThreadGoalSetParams,
) -> Result<codex::ThreadGoal> {
    let existing = load_projection(state, &params.thread_id)
        .await?
        .and_then(|projection| decode_stored_goal(&projection.goal_json).ok().flatten());
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
        tokens_used: existing.as_ref().map(|goal| goal.tokens_used).unwrap_or(0),
        time_used_seconds: existing
            .as_ref()
            .map(|goal| goal.time_used_seconds)
            .unwrap_or(0),
        created_at,
        updated_at: now,
    };
    let goal_json = serde_json::to_string(&goal).context("encoding Codex thread goal")?;
    update_goal_json(state, &params.thread_id, &goal_json).await?;
    Ok(goal.into_codex())
}

pub(in crate::commands::codex_shim) async fn get_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<codex::ThreadGoal>> {
    let projection = load_projection(state, thread_id).await?;
    projection
        .map(|projection| decode_stored_goal(&projection.goal_json))
        .transpose()
        .map(Option::flatten)
        .map(|goal| goal.map(StoredGoal::into_codex))
}

pub(in crate::commands::codex_shim) async fn clear_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<bool> {
    let had_goal = get_codex_thread_goal(state, thread_id).await?.is_some();
    update_goal_json(state, thread_id, "{}").await?;
    Ok(had_goal)
}

async fn update_goal_json(state: &ShimState, thread_id: &str, goal_json: &str) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_goal_json = escape_graphql_string(goal_json);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ goal_json: "{escaped_goal_json}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

impl StoredGoal {
    fn into_codex(self) -> codex::ThreadGoal {
        codex::ThreadGoal {
            thread_id: self.thread_id,
            objective: self.objective,
            status: self.status,
            token_budget: self.token_budget,
            tokens_used: self.tokens_used,
            time_used_seconds: self.time_used_seconds,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

fn decode_stored_goal(raw: &str) -> Result<Option<StoredGoal>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "{}" || trimmed == "null" {
        return Ok(None);
    }
    let goal =
        serde_json::from_str::<StoredGoal>(trimmed).context("decoding stored Codex thread goal")?;
    Ok(Some(goal))
}

fn now_seconds_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
