use super::*;

pub(crate) struct GoalControllerWrite {
    pub applied: bool,
    pub current: Option<GoalDocument>,
}

pub async fn update_goal_fields_if_status_from_access(
    access: &gents_store::config_client::ConfigAccess,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<bool> {
    Ok(
        update_goal_fields_if_status_observed(access, goal, expected_status, fields)
            .await?
            .applied,
    )
}

pub(crate) async fn update_goal_fields_if_status_observed(
    access: &gents_store::config_client::ConfigAccess,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<GoalControllerWrite> {
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
                    return Ok(GoalControllerWrite {
                        applied: false,
                        current,
                    });
                }
                let response = txn.execute(&mutation).await?;
                let won = response
                    .pointer("/data/update_Goal")
                    .is_some_and(mutation_returned_rows);
                if !won {
                    return Ok(GoalControllerWrite {
                        applied: false,
                        current,
                    });
                }
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
                Ok(GoalControllerWrite {
                    applied: true,
                    current: Some(updated),
                })
            })
        })
        .await
}

pub async fn claim_continuation_from_access(
    access: &gents_store::config_client::ConfigAccess,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    use gents_protocol::document_query::{fields::goal as fields, Filter, Input, Update};
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let now = Utc::now().to_rfc3339();
    let rendered = Update::matching(
        Filter::eq(fields::DOC_ID, &goal.doc_id)
            .and(Filter::eq(fields::NODE_DID, &goal.node_did))
            .and(Filter::eq(fields::STATUS, &goal.status))
            .and(Filter::eq(fields::CONTINUATION_SEQUENCE, expected_sequence)),
        Input::new()
            .set(fields::LAST_CONTINUED_FROM_REQUEST_ID, parent_request_id)
            .set(fields::CONTINUATION_SEQUENCE, next_sequence)
            .set(fields::UPDATED_AT, now),
    )
    .render();
    let response = access
        .write_rendered("goal.claim_continuation", &rendered)
        .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn claim_retry_continuation_from_access(
    access: &gents_store::config_client::ConfigAccess,
    goal: &GoalDocument,
    parent_request_id: &str,
    retry_count: i64,
    failure: &str,
) -> Result<bool> {
    use gents_protocol::document_query::{fields::goal as fields, Filter, Input, Update};
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let now = Utc::now().to_rfc3339();
    let rendered = Update::matching(
        Filter::eq(fields::DOC_ID, &goal.doc_id)
            .and(Filter::eq(fields::NODE_DID, &goal.node_did))
            .and(Filter::eq(fields::STATUS, &goal.status))
            .and(Filter::eq(fields::CONTINUATION_SEQUENCE, expected_sequence)),
        Input::new()
            .set(fields::LAST_CONTINUED_FROM_REQUEST_ID, parent_request_id)
            .set(fields::CONTINUATION_SEQUENCE, next_sequence)
            .set(fields::INFRASTRUCTURE_RETRY_COUNT, retry_count)
            .set(fields::LAST_FAILURE, failure)
            .set(fields::UPDATED_AT, now),
    )
    .render();
    let response = access
        .write_rendered("goal.claim_retry_continuation", &rendered)
        .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub(super) fn client_goal_claim_binding(
    goal: Option<&GoalDocument>,
    goal_id: Option<&str>,
    opening_verified: bool,
    opening_agent_matches: bool,
) -> bool {
    goal.is_some_and(|goal| {
        opening_verified
            && opening_agent_matches
            && goal_id == Some(goal.goal_id.as_str())
            && goal
                .assignment_root_request_doc_id
                .as_deref()
                .is_some_and(|doc| !doc.is_empty())
    })
}

#[cfg(test)]
mod client_goal_binding_tests {
    use super::*;

    #[test]
    fn generated_client_goal_claim_requires_the_physical_assignment() {
        gents_lean_vocab::families::request_input_contracts_goal_claim_binding::replay(|case| {
            let goal: GoalDocument = serde_json::from_value(serde_json::json!({
                "_docID": "physical-goal",
                "goal_id": if case.matches { "goal" } else { "other-goal" },
                "session_id": "session",
                "node_did": "node",
                "status": "active",
                "assignment_root_request_doc_id": case.root.then_some("physical-root"),
            }))
            .unwrap();
            client_goal_claim_binding(
                case.visible.then_some(&goal),
                Some("goal"),
                case.root,
                case.agent,
            )
        });
    }
}
