use super::*;

pub async fn update_goal_fields_if_status(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let expected_status = escape_graphql_string(expected_status.as_str());
    let expected_sequence = goal.continuation_sequence();
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{ {fields} }}
            ) {{ _docID }}
        }}"#
    );
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.update_controller_fields",
        |txn| {
            Box::pin(async {
                let current =
                    load_canonical_goal_in_txn(txn, &goal.node_did, &goal.session_id).await?;
                if !current
                    .as_ref()
                    .is_some_and(|current| same_goal_assignment(current, goal))
                {
                    return Ok(false);
                }
                let response = txn.execute(&mutation).await?;
                let won = response
                    .pointer("/data/update_Goal")
                    .is_some_and(mutation_returned_rows);
                if won {
                    let updated = load_canonical_goal_in_txn(txn, &goal.node_did, &goal.session_id)
                        .await?
                        .context("updated Goal row disappeared")?;
                    crate::triggers::engine::durable::publish_goal_outcomes(
                        txn,
                        &updated.node_did,
                        &updated.goal_id,
                        updated
                            .last_blocked_reason
                            .as_deref()
                            .unwrap_or(&updated.status),
                        &Utc::now().to_rfc3339(),
                    )
                    .await?;
                }
                Ok(won)
            })
        },
    )
    .await
}

pub async fn claim_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let expected_status = escape_graphql_string(&goal.status);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{
                    last_continued_from_request_id: "{parent_request_id}",
                    continuation_sequence: {next_sequence},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "goal.claim_continuation",
        &mutation,
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn claim_retry_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    parent_request_id: &str,
    retry_count: i64,
    failure: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let failure = escape_graphql_string(failure);
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let expected_status = escape_graphql_string(&goal.status);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{
                    last_continued_from_request_id: "{parent_request_id}",
                    continuation_sequence: {next_sequence},
                    infrastructure_retry_count: {retry_count},
                    last_failure: "{failure}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "goal.claim_retry_continuation",
        &mutation,
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub(crate) async fn fence_goal_continuation_claim_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &crate::watcher::AgentRequest,
    now: &str,
) -> Result<bool> {
    if request.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND) {
        return Ok(true);
    }
    let Some(goal) =
        load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id).await?
    else {
        return Ok(true);
    };
    request_head::retire_stale_assignment_continuations_in_txn(
        txn,
        &goal,
        Some(&request.doc_id),
        now,
    )
    .await
}
