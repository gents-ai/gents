use super::*;
use gents_protocol::goal::GoalClaimFence;

/// What a controller write attempt observed of the canonical Goal, in the same
/// transaction as the attempt: the post-mutation row when the mutation won,
/// otherwise the row the attempt was refused against. Either is a later and
/// stronger observation than a separate read after the transaction.
pub(crate) struct GoalControllerWrite {
    pub applied: bool,
    pub current: Option<GoalDocument>,
}

pub async fn update_goal_fields_if_status_from_access(
    access: &crate::ConfigAccess,
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
    access: &crate::ConfigAccess,
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
    access: &crate::ConfigAccess,
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
    access: &crate::ConfigAccess,
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

fn client_goal_claim_binding(
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

async fn client_goal_opening_verified_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    goal: Option<&GoalDocument>,
    agent_did: &str,
) -> Result<(bool, bool)> {
    let Some(goal) = goal else {
        return Ok((false, false));
    };
    let Some(root) = goal
        .assignment_root_request_doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
    else {
        return Ok((false, false));
    };
    use gents_protocol::document_query::{
        fields::agent_request as request, Filter, Query, Selection,
    };
    let rendered = Query::select(Selection::owned(
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
    ))
    .filter(Filter::eq(request::DOC_ID, root))
    .filter(Filter::eq(request::NODE_DID, &goal.node_did))
    .filter(Filter::eq(request::SESSION_ID, &goal.session_id))
    .limit(2)
    .render();
    let response = txn.run(&rendered).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("Goal opening query omitted rows")?,
    )?;
    let [row] = rows.as_slice() else {
        return Ok((false, false));
    };
    Ok((
        row.doc_id.as_deref() == Some(root)
            && super::request_assignment::request_belongs_to_goal(goal, row),
        row.agent_did.as_deref() == Some(agent_did),
    ))
}

pub(crate) async fn fence_goal_continuation_claim_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &gents_protocol::row::AgentRequest,
    now: &str,
    client_goal: bool,
) -> Result<GoalClaimFence> {
    if request.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND) {
        return Ok(if client_goal {
            GoalClaimFence::Rejected
        } else {
            GoalClaimFence::Allowed
        });
    }
    let goal = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id).await?;
    if client_goal {
        let (opening_verified, opening_agent_matches) =
            client_goal_opening_verified_in_txn(txn, goal.as_ref(), &request.agent_did).await?;
        if !client_goal_claim_binding(
            goal.as_ref(),
            request.caused_by_trigger_id.as_deref(),
            opening_verified,
            opening_agent_matches,
        ) {
            return Ok(GoalClaimFence::Rejected);
        }
    }
    let Some(goal) = goal else {
        return Ok(GoalClaimFence::Allowed);
    };
    let allowed = request_head::retire_stale_assignment_continuations_in_txn(
        txn,
        &goal,
        Some(&request.doc_id),
        now,
    )
    .await?;
    Ok(if allowed {
        GoalClaimFence::Allowed
    } else {
        GoalClaimFence::Retired
    })
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
        assert_eq!(cases.len(), 16);
        for case in cases {
            let visible = case["visible"].as_bool().unwrap();
            let matches = case["matches"].as_bool().unwrap();
            let root = case["root"].as_bool().unwrap();
            let agent = case["agent"].as_bool().unwrap();
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
                client_goal_claim_binding(visible.then_some(&goal), Some("goal"), root, agent),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
