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
    actor: &identity::Did,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor.clone()),
        "goal.session_usage",
        |txn| Box::pin(async move { session_token_usage_in_txn(txn, node_did, session_id).await }),
    )
    .await
}

pub(super) async fn session_token_usage_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    let usage = session_token_usage_breakdown_in_txn(txn, node_did, session_id).await?;
    Ok(i64::try_from(usage.total).unwrap_or(i64::MAX))
}

pub(crate) async fn session_token_usage_breakdown_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<SessionTokenUsage> {
    crate::tools::native::load_session_goal_usage(txn, node_did, session_id)
        .await
        .context("summing goal inference usage")
}

/// What a usage refresh observed of the canonical Goal.
pub(crate) enum GoalUsageObservation {
    /// No write was attempted because every target field already held its
    /// value, so the caller's existing observation still stands.
    Unchanged,
    /// The canonical Goal in the same transaction as the write attempt.
    Observed(Option<Box<GoalDocument>>),
}

pub async fn refresh_goal_usage_from_access(
    access: &crate::ConfigAccess,
    goal: &GoalDocument,
) -> Result<i64> {
    Ok(refresh_goal_usage_observed(access, goal, None).await?.0)
}

/// `charged` is the session's charged total read in the caller's own
/// transaction; without one the scan runs here.
pub(crate) async fn refresh_goal_usage_observed(
    access: &crate::ConfigAccess,
    goal: &GoalDocument,
    charged: Option<i64>,
) -> Result<(i64, GoalUsageObservation)> {
    let charged = match charged {
        Some(charged) => charged,
        None => {
            access
                .transact_readonly("goal.session_usage", |txn| {
                    Box::pin(async move {
                        session_token_usage_in_txn(txn, &goal.node_did, &goal.session_id).await
                    })
                })
                .await?
        }
    };
    let tokens = charged
        .saturating_sub(goal.token_usage_baseline.unwrap_or_default().max(0))
        .max(0);
    let status = goal
        .parsed_status()
        .context("Goal usage snapshot has an unknown status")?;
    // `active_time_seconds` is derived at read time from `active_started_at`,
    // so rewriting both carries nothing a reader cannot recompute; only the
    // charged total and the accrual flag can change what is observable.
    if tokens == goal.tokens_used.unwrap_or_default().max(0)
        && goal.active_started_at.is_some() == status.accrues_active_time()
    {
        return Ok((tokens, GoalUsageObservation::Unchanged));
    }
    let now = Utc::now();
    let now_string = now.to_rfc3339();
    let active_time = goal.current_active_time_seconds(now);
    let updated_at = escape_graphql_string(&now_string);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now_string.as_str()),
    );
    let observed = super::controller::update_goal_fields_if_status_observed(
        access,
        goal,
        status,
        &format!(
            "tokens_used: {tokens}, active_time_seconds: {active_time}, {active_started_field} updated_at: \"{updated_at}\""
        ),
    )
    .await?;
    Ok((
        tokens,
        GoalUsageObservation::Observed(observed.current.map(Box::new)),
    ))
}
