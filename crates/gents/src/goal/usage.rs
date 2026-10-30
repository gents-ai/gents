use super::*;

/// Tokens charged against a session's budget. `input` and `output` are the
/// durable `InferenceCall` prompt and completion columns; `total` is their
/// charged sum (`crate::provider_usage::sum_charged_from_persisted_parts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

pub async fn session_token_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "goal.session_usage",
        |txn| Box::pin(async move { session_token_usage_in_txn(txn, node_did, session_id).await }),
    )
    .await
}

pub(super) async fn session_token_usage_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    let usage = session_token_usage_breakdown_in_txn(txn, node_did, session_id).await?;
    Ok(i64::try_from(usage.total).unwrap_or(i64::MAX))
}

pub(crate) async fn session_token_usage_breakdown_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<SessionTokenUsage> {
    crate::toolset::load_session_goal_usage(txn, node_did, session_id)
        .await
        .context("summing goal inference usage")
}

pub async fn refresh_goal_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
) -> Result<i64> {
    let tokens = session_token_usage(node, &goal.node_did, &goal.session_id)
        .await?
        .saturating_sub(goal.token_usage_baseline.unwrap_or_default().max(0))
        .max(0);
    let now = Utc::now();
    let now_string = now.to_rfc3339();
    let active_time = goal.current_active_time_seconds(now);
    let updated_at = escape_graphql_string(&now_string);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        goal.parsed_status()
            .is_some_and(GoalStatus::accrues_active_time)
            .then_some(now_string.as_str()),
    );
    let status = goal
        .parsed_status()
        .context("Goal usage snapshot has an unknown status")?;
    update_goal_fields_if_status(
        node,
        goal,
        status,
        &format!(
            "tokens_used: {tokens}, active_time_seconds: {active_time}, {active_started_field} updated_at: \"{updated_at}\""
        ),
    )
    .await?;
    Ok(tokens)
}
