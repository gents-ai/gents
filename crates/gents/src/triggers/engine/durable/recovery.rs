use super::{
    assignment_replaced, observe_goal_outcome_binding, outcome_published, stage_outcome,
    OutcomeStage, FIRE_FIELDS,
};
use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
use gents_protocol::trigger_delivery::TriggerFire;

#[cfg(test)]
pub(crate) async fn recover_outcomes(
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    agent_did: &str,
) -> Result<usize> {
    let actor = identity::Did::new(agent_did)?;
    let access = crate::store::config_client::ConfigAccess::local_as(node.clone(), actor.clone());
    recover_outcomes_with_access(&access, owner, agent_did).await
}

pub(crate) async fn recover_outcomes_with_access(
    access: &crate::ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<usize> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent_did),
        "trigger recovery actor differs from access actor"
    );
    let response = access.execute(&format!(
        r#"{{TriggerFire(filter: {{owner_did: {{_eq: "{}"}}, emit_outcome: {{_eq: true}}}}) {{{FIRE_FIELDS}}}}}"#,
        crate::store::graphql::escape_graphql_string(owner)
    )).await?;
    let fires: Vec<TriggerFire> = serde_json::from_value(response["data"]["TriggerFire"].clone())?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut recovered = 0;
    for fire in fires {
        recovered += access
            .transact_write("trigger.recover_outcomes", |txn| {
                let (fire, now) = (&fire, &now);
                Box::pin(async move {
                    recover_fire_outcome_in_txn(txn, owner, fire, now, Some(agent_did)).await
                })
            })
            .await?;
    }
    Ok(recovered)
}

#[cfg(test)]
pub(crate) async fn recover_outcomes_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
) -> Result<usize> {
    let response = txn.execute_local_response(&format!(
        r#"{{TriggerFire(filter: {{owner_did: {{_eq: "{}"}}, emit_outcome: {{_eq: true}}}}) {{{FIRE_FIELDS}}}}}"#,
        crate::store::graphql::escape_graphql_string(owner)
    )).await?;
    let fires: Vec<TriggerFire> = crate::store::graphql::rows(&response, "TriggerFire")?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut recovered = 0;
    for fire in fires {
        recovered += recover_fire_outcome_in_txn(txn, owner, &fire, &now, None).await?;
    }
    Ok(recovered)
}

async fn recover_fire_outcome_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    fire: &TriggerFire,
    now: &str,
    agent_did: Option<&str>,
) -> Result<usize> {
    if outcome_published(txn, fire).await? {
        return Ok(0);
    }
    let response = txn
        .execute_local_response(&format!(
            r#"{{AgentRequest(filter: {{
        node_did: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}
    }}) {{request_id agent_did lifecycle_state failure_reason}}}}"#,
            crate::store::graphql::escape_graphql_string(owner),
            crate::store::graphql::escape_graphql_string(&fire.request_id)
        ))
        .await?;
    let requests: Vec<gents_protocol::row::AgentRequestRow> =
        crate::store::graphql::rows(&response, "AgentRequest")?;
    if requests.is_empty() && agent_did.is_some() {
        return Ok(0);
    }
    anyhow::ensure!(
        requests.len() == 1,
        "Task fire has no unique admitted request during outcome recovery"
    );
    let request = &requests[0];
    if agent_did.is_some_and(|agent| request.agent_did.as_deref() != Some(agent)) {
        return Ok(0);
    }
    let status = request
        .lifecycle_state
        .context("Task request is missing lifecycle state")?;
    let binding = if fire.goal_assignment_applied {
        observe_goal_outcome_binding(txn, fire).await?
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
    Ok(usize::from(
        stage_outcome(
            txn,
            fire,
            OutcomeStage {
                goal: binding.as_ref(),
                assignment_replaced: assignment_replaced(fire, binding.as_ref()),
                request_terminal: status.is_terminal(),
                terminal_state: status.as_str(),
                reason,
            },
            now,
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::config_client::ConfigAccess;
    use crate::triggers::engine::durable::{fire_key, stage_fire_request};

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
            let node = std::sync::Arc::new(
                crate::test_support::node_builder()
                    .data_path(directory.path())
                    .build()
                    .await
                    .unwrap(),
            );
            crate::ensure_runtime_schemas(&node).await.unwrap();
            let terminal = modeled["terminal"].as_bool().unwrap();
            let state = if terminal { "completed" } else { "pending" };
            let agent_did = crate::triggers::callback::test_agent_did();
            let request_mutation = format!(
                r#"mutation {{create_AgentRequest(input: {{
                request_id: "{}", node_did: "{}", session_id: "{}", agent_did: "{}",
                purpose: "normal", lifecycle_state: "{state}", created_at: "2026-01-01T00:00:00Z"
            }}) {{_docID}} }}"#,
                crate::store::graphql::escape_graphql_string(&fire.request_id),
                crate::store::graphql::escape_graphql_string(&fire.identity.owner_did),
                crate::store::graphql::escape_graphql_string(&session),
                crate::store::graphql::escape_graphql_string(&agent_did)
            );
            ConfigAccess::transact_local_write(&node, None, "test.seed_terminal_outcome_boundary", |txn| {
                let fire = &fire;
                let request_mutation = &request_mutation;
                Box::pin(async move {
                    stage_fire_request(txn, fire, request_mutation).await?;
                    if goal_backed {
                        let response = txn.execute(&format!("{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID}}}}", crate::store::graphql::escape_graphql_string(&fire.request_id))).await?;
                        let root = response["data"]["AgentRequest"][0]["_docID"].as_str().unwrap();
                        txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                            &serde_json::json!({"input": {
                                "goal_id": fire.goal_id, "node_did": fire.identity.owner_did,
                                "session_id": fire.session_id, "objective": "finish assignment",
                                "status": case["pre"]["goals"][0]["status"], "assignment_root_request_doc_id": root,
                            }})).await?;
                    }
                    if !case["pre"]["outcomes"].as_array().unwrap().is_empty() {
                        stage_outcome(txn, fire, OutcomeStage {
                            goal: None, assignment_replaced: false, request_terminal: terminal,
                            terminal_state: state, reason: state,
                        }, "2026-01-01T00:00:01Z").await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
            node.shutdown().await.expect("node shutdown");
            drop(node);
            let node = std::sync::Arc::new(
                EmbeddedNode::builder()
                    .data_path(directory.path())
                    .build()
                    .await
                    .unwrap(),
            );
            let expected = case["post"]["outcomes"].as_array().unwrap().len();
            let before = case["pre"]["outcomes"].as_array().unwrap().len();
            assert_eq!(
                recover_outcomes(&node, &fire.identity.owner_did, &agent_did)
                    .await
                    .unwrap(),
                expected - before,
                "{name}"
            );
            assert_eq!(
                recover_outcomes(&node, &fire.identity.owner_did, &agent_did)
                    .await
                    .unwrap(),
                0,
                "{name}"
            );
            let response = crate::store::graphql::graphql_with_transaction_retry(
                &node,
                "{ FireOutcome { handoff_id } }",
                "verify recovered outcome uniqueness",
            )
            .await
            .unwrap();
            let outcomes: Vec<serde_json::Value> =
                crate::store::graphql::rows(&response, "FireOutcome").unwrap();
            assert_eq!(outcomes.len(), expected, "{name}");
            node.shutdown().await.expect("node shutdown");
        }
    }
}
