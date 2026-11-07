use super::*;

/// Set the session's Goal as its working Agent, signed by that Agent's managed
/// key. The session is resolved inside the Agent's own transaction, so a caller
/// holding another Agent's key, or none, cannot select the session's Agent by
/// an unauthenticated read; such a call fails rather than writing as anyone else.
pub async fn set_goal(
    node: &std::sync::Arc<EmbeddedNode>,
    agent: &dyn crate::store::identity::NodeIdentity,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let access = crate::store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent,
    )?;
    let goal = access
        .transact_write("goal.set", move |txn| {
            Box::pin(async move {
                let session =
                    crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                        .await?
                        .context("cannot set a Goal for a missing session")?;
                anyhow::ensure!(
                    Some(session.session.agent_did.as_str()) == txn.actor_did(),
                    "only the session's working Agent may set its Goal through this owner"
                );
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
        .await?;
    super::creation::finish_goal_relationships(&access, node_did, session_id).await?;
    Ok(goal)
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
    let goal = access
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
        .await?;
    super::creation::finish_goal_relationships(access, node_did, session_id).await?;
    Ok(goal)
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
        let mut input = serde_json::json!({
            "objective": objective,
            "status": post.status.as_str(),
            "token_budget": budget,
            "tokens_used": existing.tokens_used.unwrap_or_default().max(0),
            "active_time_seconds": active_time,
            "active_started_at": post.status.accrues_active_time().then_some(now_string.as_str()),
            "consecutive_blocked_audits": post.blocked_audits,
            "wrapup_requested": post.wrapup_requested,
            "wrapup_completed": post.wrapup_completed,
            "updated_at": now_string,
        });
        if let Some(enabled) = auto_resume {
            input["auto_resume_at_reset"] = enabled.into();
        }
        let doc_id = escape_graphql_string(&existing.doc_id);
        let escaped_node_did = escape_graphql_string(node_did);
        let response = txn.execute_client_signed_update(
            &format!(r#"mutation($input: GoalMutationInputArg!) {{
                update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{escaped_node_did}" }} }}, input: $input) {{ _docID }}
            }}"#),
            &serde_json::json!({"input": input}),
            gents_protocol::schemas::GOAL,
        ).await?;
        anyhow::ensure!(
            response["data"]["update_Goal"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"] == existing.doc_id),
            "Goal update did not retain its physical identity"
        );
        let updated = load_canonical_goal_in_txn(txn, node_did, session_id)
            .await?
            .context("updated Goal row disappeared")?;
        crate::triggers::engine::durable::publish_goal_outcomes(
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
    let session = crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
        .await?
        .context("goal creation requires a visible physical session")?;
    let creator = txn
        .actor_did()
        .context("goal creation requires an authenticated actor")?;
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    super::creation::create_goal_child_in_txn(
        txn,
        "Goal",
        serde_json::json!({
            "goal_id": goal_id,
            "session_id": session_id,
            "session_doc_id": session.doc_id,
            "agent_did": session.session.agent_did,
            "creator_did": creator,
            "parent_linked": true,
            "requester_did": session.session.requester_did,
            "node_did": node_did,
            "objective": objective,
            "status": status.as_str(),
            "token_budget": budget,
            "tokens_used": 0,
            "token_usage_baseline": baseline,
            "active_time_seconds": 0,
            "active_started_at": status.accrues_active_time().then_some(now_string.as_str()),
            "consecutive_blocked_audits": initial_state.blocked_audits,
            "continuation_sequence": 0,
            "wrapup_requested": initial_state.wrapup_requested,
            "wrapup_completed": initial_state.wrapup_completed,
            "infrastructure_retry_count": 0,
            "auto_resume_at_reset": auto_resume,
            "created_at": now_string,
            "updated_at": now_string,
        }),
        &session.doc_id,
        &session.session.agent_did,
    )
    .await?;
    let created = load_canonical_goal_in_txn(txn, node_did, session_id)
        .await?
        .context("created Goal row not found")?;
    Ok(created)
}

/// Delete the exact Goal through the caller's held native authority.
pub async fn delete_goal_from_access(
    access: &crate::ConfigAccess,
    goal: &GoalDocument,
) -> Result<bool> {
    access.transact_write("goal.delete", |txn| {
        Box::pin(async move {
            let doc = escape_graphql_string(&goal.doc_id);
            let owner = escape_graphql_string(&goal.node_did);
            let session = escape_graphql_string(&goal.session_id);
            let response = txn.execute(&format!(r#"{{ Goal(filter: {{_docID: {{_eq: "{doc}"}}, node_did: {{_eq: "{owner}"}}, session_id: {{_eq: "{session}"}}}}, limit: 2) {{_docID}} }}"#)).await?;
            let rows = response["data"]["Goal"].as_array().context("Goal delete read omitted rows")?;
            anyhow::ensure!(rows.len() <= 1, "Goal delete target is ambiguous");
            if rows.is_empty() { return Ok::<_, anyhow::Error>(false); }
            txn.delete_configuration_document("Goal", &goal.doc_id).await?;
            Ok(true)
        })
    }).await
}

/// Delete only visible Goal and claim documents through the caller's held authority.
pub async fn delete_goals_for_session_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> Result<usize> {
    access.transact_write("goal.delete_for_session", |txn| {
        Box::pin(async move {
            let owner = escape_graphql_string(node_did);
            let session = escape_graphql_string(session_id);
            let response = txn.execute(&format!(r#"{{
                Goal(filter: {{node_did: {{_eq: "{owner}"}}, session_id: {{_eq: "{session}"}}}}) {{_docID}}
                GoalCreationClaim(filter: {{node_did: {{_eq: "{owner}"}}, session_id: {{_eq: "{session}"}}}}) {{_docID}}
            }}"#)).await?;
            let mut goals = 0;
            for collection in ["Goal", "GoalCreationClaim"] {
                let rows = response["data"][collection].as_array().context("Goal delete read omitted rows")?;
                for row in rows {
                    let doc = row["_docID"].as_str().context("Goal delete target has no physical ID")?;
                    txn.delete_configuration_document(collection, doc).await?;
                    if collection == "Goal" { goals += 1; }
                }
            }
            Ok::<_, anyhow::Error>(goals)
        })
    }).await
}
