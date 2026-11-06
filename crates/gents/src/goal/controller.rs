use super::*;

pub async fn update_goal_fields_if_status(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &identity::Did,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<bool> {
    update_goal_fields_if_status_from_access(
        &crate::ConfigAccess::local_as(node.clone(), actor.clone()),
        goal,
        expected_status,
        fields,
    )
    .await
}

pub(crate) async fn update_goal_fields_if_status_from_access(
    access: &crate::ConfigAccess,
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
    access
        .transact_write("goal.update_controller_fields", |txn| {
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
        })
        .await
}

pub async fn claim_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &identity::Did,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    claim_continuation_from_access(
        &crate::ConfigAccess::local_as(node.clone(), actor.clone()),
        goal,
        parent_request_id,
    )
    .await
}

pub(crate) async fn claim_continuation_from_access(
    access: &crate::ConfigAccess,
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
    let response = access.write("goal.claim_continuation", &mutation).await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn claim_retry_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &identity::Did,
    goal: &GoalDocument,
    parent_request_id: &str,
    retry_count: i64,
    failure: &str,
) -> Result<bool> {
    claim_retry_continuation_from_access(
        &crate::ConfigAccess::local_as(node.clone(), actor.clone()),
        goal,
        parent_request_id,
        retry_count,
        failure,
    )
    .await
}

pub(crate) async fn claim_retry_continuation_from_access(
    access: &crate::ConfigAccess,
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
    let response = access
        .write("goal.claim_retry_continuation", &mutation)
        .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub(crate) async fn fence_goal_continuation_claim_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &crate::request::watcher::AgentRequest,
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

#[cfg(test)]
mod client_goal_binding_tests {
    use super::*;

    #[test]
    fn generated_client_goal_claim_requires_the_physical_assignment() {
        let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
        let cases = snapshot.request_input_cases[0]["client_goal_claim_binding_cases"]
            .as_array()
            .unwrap();
        assert_eq!(cases.len(), 8);
        for case in cases {
            let visible = case["visible"].as_bool().unwrap();
            let matches = case["matches"].as_bool().unwrap();
            let root = case["root"].as_bool().unwrap();
            let goal: GoalDocument = serde_json::from_value(serde_json::json!({
                "_docID": "physical-goal",
                "goal_id": if matches { "goal" } else { "other-goal" },
                "session_id": "session",
                "node_did": "node",
                "status": "active",
                "assignment_root_request_doc_id": root.then_some("physical-root"),
            }))
            .unwrap();
            assert_eq!(
                client_goal_claim_binding(visible.then_some(&goal), Some("goal")),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
