use super::{assignment_replaced, observe_goal_outcome_binding, stage_outcome, FIRE_FIELDS};
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::trigger_delivery::TriggerFire;

/// Startup and crash-boundary tests use the same transaction owner. Terminal
/// facts and assignment bindings are observed in the publication snapshot.
pub(crate) async fn recover_outcomes(node: &EmbeddedNode, owner: &str) -> Result<usize> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "trigger.recover_outcomes",
        |txn| Box::pin(async move { recover_outcomes_in_txn(txn, owner).await }),
    )
    .await
}

pub(crate) async fn recover_outcomes_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
) -> Result<usize> {
    let response = txn.execute_local_response(&format!(
        r#"{{TriggerFire(filter: {{owner_did: {{_eq: "{}"}}, emit_outcome: {{_eq: true}}}}) {{{FIRE_FIELDS}}}}}"#,
        crate::graphql::escape_graphql_string(owner)
    )).await?;
    let fires: Vec<TriggerFire> = crate::graphql::rows(&response, "TriggerFire")?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut recovered = 0;
    for fire in fires {
        let response = txn
            .execute_local_response(&format!(
                r#"{{AgentRequest(filter: {{
            node_did: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}
        }}) {{request_id lifecycle_state failure_reason}}}}"#,
                crate::graphql::escape_graphql_string(owner),
                crate::graphql::escape_graphql_string(&fire.request_id)
            ))
            .await?;
        let requests: Vec<gents_protocol::row::AgentRequestRow> =
            crate::graphql::rows(&response, "AgentRequest")?;
        anyhow::ensure!(
            requests.len() == 1,
            "Task fire has no unique admitted request during outcome recovery"
        );
        let request = &requests[0];
        let status = request
            .lifecycle_state
            .context("Task request is missing lifecycle state")?;
        let binding = if fire.goal_assignment_applied {
            observe_goal_outcome_binding(txn, &fire).await?
        } else {
            None
        };
        let reason = binding
            .as_ref()
            .map(|goal| goal.status.as_str())
            .unwrap_or_else(|| {
                request
                    .failure_reason
                    .as_deref()
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or(status.as_str())
            });
        recovered += usize::from(
            stage_outcome(
                txn,
                &fire,
                binding.as_ref(),
                assignment_replaced(&fire, binding.as_ref()),
                status.is_terminal(),
                status.as_str(),
                reason,
                &now,
            )
            .await?,
        );
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::trigger_engine::durable::{fire_key, stage_fire_request};

    #[tokio::test]
    async fn restart_outcome_recovery_matches_modeled_crash_boundaries() {
        let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
        for name in [
            "crash_after_terminal_before_outcome",
            "crash_after_outcome_retry",
            "continuing_goal_has_no_outcome",
        ] {
            let case = contract["trigger_delivery"]["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == name)
                .unwrap();
            let modeled = &case["pre"]["requests"][0];
            let modeled_fire = &modeled["fire"];
            let identity: gents_protocol::trigger_delivery::FireIdentity =
                serde_json::from_value(modeled_fire["identity"].clone()).unwrap();
            let key = fire_key(&identity);
            let goal_backed = modeled_fire["goal_backed"].as_bool().unwrap();
            let session = modeled_fire["session"].as_str().unwrap().to_owned();
            let fire = TriggerFire {
                fire_key: key.clone(),
                identity: identity.clone(),
                task_id: "recovery-task".into(),
                request_id: identity.request_id(),
                session_id: session.clone(),
                goal_id: goal_backed.then(|| "recovery-goal".into()),
                goal_objective: goal_backed.then(|| "finish assignment".into()),
                goal_token_budget: None,
                goal_assignment_applied: modeled["goal_assignment_applied"].as_bool().unwrap(),
                emit_outcome: modeled_fire["emit_outcome"].as_bool().unwrap(),
                queued_serial: modeled_fire["serial"].as_bool().unwrap(),
                source_handoff_id: Some("source-assignment".into()),
                reply_session_id: None,
                shard_id: None,
                attempt: None,
                created_at: "2026-01-01T00:00:00Z".into(),
            };
            let directory = tempfile::tempdir().unwrap();
            let node = EmbeddedNode::builder()
                .data_path(directory.path())
                .build()
                .await
                .unwrap();
            crate::ensure_runtime_schemas(&node).await.unwrap();
            let terminal = modeled["terminal"].as_bool().unwrap();
            let state = if terminal { "completed" } else { "pending" };
            let request_mutation = format!(
                r#"mutation {{create_AgentRequest(input: {{
                request_id: "{}", node_did: "{}", session_id: "{}", agent_id: "behavior",
                purpose: "normal", lifecycle_state: "{state}", created_at: "2026-01-01T00:00:00Z"
            }}) {{_docID}} }}"#,
                crate::graphql::escape_graphql_string(&fire.request_id),
                crate::graphql::escape_graphql_string(&fire.identity.owner_did),
                crate::graphql::escape_graphql_string(&session)
            );
            ConfigAccess::transact_local(&node, None, "test.seed_terminal_outcome_boundary", |txn| {
                let fire = &fire;
                let request_mutation = &request_mutation;
                Box::pin(async move {
                    stage_fire_request(txn, fire, request_mutation).await?;
                    if goal_backed {
                        let response = txn.execute(&format!("{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID}}}}", crate::graphql::escape_graphql_string(&fire.request_id))).await?;
                        let root = response["data"]["AgentRequest"][0]["_docID"].as_str().unwrap();
                        txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                            &serde_json::json!({"input": {
                                "goal_id": fire.goal_id, "node_did": fire.identity.owner_did,
                                "session_id": fire.session_id, "objective": "finish assignment",
                                "status": case["pre"]["goals"][0]["status"], "assignment_root_request_doc_id": root,
                            }})).await?;
                    }
                    if !case["pre"]["outcomes"].as_array().unwrap().is_empty() {
                        stage_outcome(txn, fire, None, false, terminal,
                            state, state, "2026-01-01T00:00:01Z").await?;
                    }
                    Ok(())
                })
            }).await.unwrap();
            node.shutdown().await;
            drop(node);
            let node = EmbeddedNode::builder()
                .data_path(directory.path())
                .build()
                .await
                .unwrap();
            let expected = case["post"]["outcomes"].as_array().unwrap().len();
            let before = case["pre"]["outcomes"].as_array().unwrap().len();
            assert_eq!(
                recover_outcomes(&node, &fire.identity.owner_did)
                    .await
                    .unwrap(),
                expected - before,
                "{name}"
            );
            assert_eq!(
                recover_outcomes(&node, &fire.identity.owner_did)
                    .await
                    .unwrap(),
                0,
                "{name}"
            );
            let response = crate::graphql::graphql_with_transaction_retry(
                &node,
                "{ FireOutcome { handoff_id } }",
                "verify recovered outcome uniqueness",
            )
            .await
            .unwrap();
            let outcomes: Vec<serde_json::Value> =
                crate::graphql::rows(&response, "FireOutcome").unwrap();
            assert_eq!(outcomes.len(), expected, "{name}");
            node.shutdown().await;
        }
    }
}
