use super::*;

pub async fn set_goal(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    crate::config_client::ConfigAccess::transact_local_write(node, None, "goal.set", move |txn| {
        Box::pin(async move {
            set_goal_in_txn(
                txn,
                node_did,
                session_id,
                objective,
                status,
                token_budget,
                None,
            )
            .await
        })
    })
    .await
}

pub async fn set_goal_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
    auto_resume: Option<bool>,
) -> Result<GoalDocument> {
    access
        .transact_write("goal.set_from_access", move |txn| {
            Box::pin(async move {
                set_goal_in_txn(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    status,
                    token_budget,
                    auto_resume,
                )
                .await
            })
        })
        .await
}

/// An operator reset preference belongs to the observed Goal, including when
/// another assignment replaces the session's Goal before the write arrives.
pub async fn set_goal_auto_resume_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    expected_goal_id: &str,
    enabled: bool,
) -> Result<()> {
    access
        .transact_write("goal.set_auto_resume", |txn| {
            Box::pin(async move {
                let goal = load_canonical_goal_in_txn(txn, node_did, session_id)
                    .await?
                    .context("Goal no longer exists")?;
                anyhow::ensure!(
                    goal.goal_id == expected_goal_id,
                    "Goal changed before its reset preference was saved"
                );
                set_goal_in_txn(txn, node_did, session_id, None, None, None, Some(enabled)).await?;
                Ok(())
            })
        })
        .await
}

pub(crate) async fn pause_goal_for_interrupt_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<()> {
    let Some(goal) = load_canonical_goal_in_txn(txn, node_did, session_id).await? else {
        return Ok(());
    };
    let state = goal.state().context("Goal has an unknown status")?;
    if state.step(GoalAction::Pause).is_some() {
        set_goal_in_txn(
            txn,
            node_did,
            session_id,
            None,
            Some(GoalStatus::Paused),
            None,
            None,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn set_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
    auto_resume: Option<bool>,
) -> Result<GoalDocument> {
    anyhow::ensure!(
        objective.is_none_or(|value| !value.trim().is_empty()),
        "goal objective must be non-empty"
    );
    let existing = load_canonical_goal_in_txn(txn, node_did, session_id).await?;
    let objective = objective
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| existing.as_ref().map(|goal| goal.objective.clone()))
        .context("a goal objective is required")?;
    let status = status
        .or_else(|| existing.as_ref().and_then(GoalDocument::parsed_status))
        .unwrap_or(GoalStatus::Active);
    let budget =
        token_budget.unwrap_or_else(|| existing.as_ref().and_then(|goal| goal.token_budget));
    if budget.is_some_and(|value| value <= 0) {
        bail!("goal token budget must be positive");
    }
    let auto_resume_field = auto_resume
        .map(|on| format!("auto_resume_at_reset: {on},"))
        .unwrap_or_default();

    let now = Utc::now();
    let now_string = now.to_rfc3339();
    if let Some(existing) = existing {
        let pre = existing
            .state()
            .context("existing Goal has an unknown status")?;
        anyhow::ensure!(
            pre.status == GoalStatus::Active || status != GoalStatus::Active,
            "reactivating an existing goal requires goal resume-request --from REQUEST_ID"
        );
        let post = apply_operator_status_transition(pre, status)?;
        let active_time = existing.current_active_time_seconds(now);
        let active_started_at = if post.status.accrues_active_time() {
            now_string.clone()
        } else {
            String::new()
        };
        let budget_field = optional_int_graphql_field("token_budget", budget);
        let active_started_field = optional_string_graphql_field(
            "active_started_at",
            (!active_started_at.is_empty()).then_some(active_started_at.as_str()),
        );
        let doc_id = escape_graphql_string(&existing.doc_id);
        let escaped_node_did = escape_graphql_string(node_did);
        let objective = escape_graphql_string(&objective);
        let status = post.status.as_str();
        let now = escape_graphql_string(&now_string);
        let mutation = format!(
            r#"mutation {{
                update_Goal(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{escaped_node_did}" }} }},
                    input: {{
                        objective: "{objective}",
                        status: "{status}",
                        {budget_field}
                        tokens_used: {tokens_used},
                        active_time_seconds: {active_time},
                        {active_started_field}
                        consecutive_blocked_audits: {blocked_audits},
                        wrapup_requested: {wrapup_requested},
                        wrapup_completed: {wrapup_completed},
                        {auto_resume_field}
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            tokens_used = existing.tokens_used.unwrap_or_default().max(0),
            blocked_audits = post.blocked_audits,
            wrapup_requested = post.wrapup_requested,
            wrapup_completed = post.wrapup_completed,
        );
        txn.execute(&mutation).await?;
        let updated = load_canonical_goal_in_txn(txn, node_did, session_id)
            .await?
            .context("updated Goal row disappeared")?;
        crate::trigger_engine::durable::publish_goal_outcomes(
            txn,
            node_did,
            &updated.goal_id,
            updated
                .last_blocked_reason
                .as_deref()
                .unwrap_or(&updated.status),
            &now_string,
        )
        .await?;
        return Ok(updated);
    }

    let initial_state = GoalState {
        status,
        blocked_audits: if status == GoalStatus::Blocked {
            BLOCKED_AUDIT_THRESHOLD
        } else {
            0
        },
        wrapup_requested: status == GoalStatus::BudgetLimited,
        wrapup_completed: status == GoalStatus::Complete,
    };
    let goal_id = deterministic_goal_id(node_did, session_id);
    let escaped_goal_id = escape_graphql_string(&goal_id);
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_objective = escape_graphql_string(&objective);
    let escaped_now = escape_graphql_string(&now_string);
    let budget_field = optional_int_graphql_field("token_budget", budget);
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now_string.as_str()),
    );
    let mutation = format!(
        r#"mutation {{
            create_Goal(input: {{
                goal_id: "{escaped_goal_id}",
                session_id: "{escaped_session_id}",
                node_did: "{escaped_node_did}",
                objective: "{escaped_objective}",
                status: "{status}",
                {budget_field}
                tokens_used: 0,
                token_usage_baseline: {baseline},
                active_time_seconds: 0,
                {active_started_field}
                consecutive_blocked_audits: {blocked_audits},
                continuation_sequence: 0,
                wrapup_requested: {wrapup_requested},
                wrapup_completed: {wrapup_completed},
                infrastructure_retry_count: 0,
                {auto_resume_field}
                created_at: "{escaped_now}",
                updated_at: "{escaped_now}"
            }}) {{ _docID }}
        }}"#,
        status = status.as_str(),
        blocked_audits = initial_state.blocked_audits,
        wrapup_requested = initial_state.wrapup_requested,
        wrapup_completed = initial_state.wrapup_completed,
    );
    txn.execute(&mutation).await?;
    let created = load_canonical_goal_in_txn(txn, node_did, session_id)
        .await?
        .context("created Goal row not found")?;
    Ok(created)
}

pub async fn delete_goal(node: &std::sync::Arc<EmbeddedNode>, goal: &GoalDocument) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let mutation = format!(
        r#"mutation {{
            delete_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }} }}) {{ _docID }}
        }}"#
    );
    let response =
        crate::config_client::ConfigAccess::write_local_response(node, "goal.delete", &mutation)
            .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn delete_goals_for_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<usize> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let node_did = &node_did;
    let session_id = &session_id;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.delete_for_session",
        move |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"mutation {{
            delete_Goal(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                session_id: {{ _eq: "{session_id}" }}
            }}) {{ _docID }}
        }}"#
                    ))
                    .await?;
                txn.execute(&format!(
                    r#"mutation {{
                delete_GoalCreationClaim(filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }}
                }}) {{ _docID }}
            }}"#
                ))
                .await?;
                Ok::<_, anyhow::Error>(
                    response
                        .pointer("/data/delete_Goal")
                        .map(mutation_row_count)
                        .unwrap_or_default(),
                )
            })
        },
    )
    .await
}
