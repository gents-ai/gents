use super::*;
use gents_protocol::session_token_usage::SessionTokenUsage;

pub async fn session_token_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &identity::Did,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor.clone()),
        "goal.session_usage",
        |txn| Box::pin(async move { session_token_usage_in_txn(txn, node_did, session_id).await }),
    )
    .await
}

pub(super) async fn session_token_usage_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    let usage = session_token_usage_breakdown_in_txn(txn, node_did, session_id).await?;
    Ok(i64::try_from(usage.total).unwrap_or(i64::MAX))
}

pub(crate) async fn session_token_usage_breakdown_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<SessionTokenUsage> {
    gents_tools::native::load_session_goal_usage(txn, node_did, session_id)
        .await
        .context("summing goal inference usage")
}

pub(crate) enum GoalUsageObservation {
    Unchanged,
    Observed(Option<Box<GoalDocument>>),
}

pub async fn refresh_goal_usage_from_access(
    access: &gents_store::config_client::ConfigAccess,
    goal: &GoalDocument,
) -> Result<i64> {
    Ok(refresh_goal_usage_observed(access, goal, None).await?.0)
}

pub(crate) async fn refresh_goal_usage_observed(
    access: &gents_store::config_client::ConfigAccess,
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
