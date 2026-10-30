use super::*;
use crate::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::{json, Value};
use std::sync::Arc;

async fn signed_request(
    access: &ConfigAccess,
    identity: &KeyIdentity,
    id: &str,
    session: &str,
    goal_id: &str,
) -> String {
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        identity.did(),
        identity.did(),
        "task-agent",
        session,
        "Task assignment input",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: goal_id.into(),
        root_request_doc_id: None,
    });
    crate::sign_agent_request_create(identity, &mut request)
        .await
        .unwrap();
    let response = access
        .write(
            "test.retained_fire_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    access
        .write(
            "test.retained_fire_terminal",
            &format!(
                r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
                escape_graphql_string(&doc)
            ),
        )
        .await
        .unwrap();
    doc
}

async fn goal_document(access: &ConfigAccess, input: Value) -> String {
    let result = access
        .transact_write("test.retained_fire_goal", |txn| {
            let input = input.clone();
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                    &json!({"input":input}),
                ).await?;
                crate::graphql::created_doc_id(&response, "Goal")
            })
        })
        .await
        .unwrap();
    result
}

async fn outcome_rows(access: &ConfigAccess) -> Vec<Value> {
    access
        .execute("{FireOutcome {handoff_id fire_key request_id session_id goal_id terminal_state reason source_handoff_id created_at}}")
        .await.unwrap()["data"]["FireOutcome"].as_array().unwrap().clone()
}

#[tokio::test]
async fn retained_task_outcomes_follow_modeled_assignment_history() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    let cases = contract["trigger_delivery"]["retained_outcomes"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 15);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(directory.path().join("owner.key"), None).unwrap();
        let other = KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
        let path = directory.path().join("store");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .data_path(&path)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let session = case["request"]["fire"]["session"].as_str().unwrap();
        let mut fire_identity: FireIdentity =
            serde_json::from_value(case["request"]["fire"]["identity"].clone()).unwrap();
        fire_identity.owner_did = identity.did().into();
        let fire = TriggerFire {
            fire_key: fire_identity.fire_key(),
            request_id: fire_identity.request_id(),
            identity: fire_identity,
            task_id: "retained-task".into(),
            session_id: session.into(),
            goal_id: Some("task-goal".into()),
            goal_objective: Some("Original Task assignment".into()),
            goal_token_budget: None,
            goal_assignment_applied: true,
            emit_outcome: true,
            queued_serial: true,
            source_handoff_id: Some("retained-source-handoff".into()),
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-10-21T00:00:00Z".into(),
        };
        let root = signed_request(&access, &identity, &fire.request_id, session, "task-goal").await;
        let reused = case["classification"] == "superseded";
        let next_root = signed_request(
            &access,
            &identity,
            "fresh-request",
            session,
            if reused { "task-goal" } else { "fresh-goal" },
        )
        .await;
        access
            .transact_write("test.retained_fire_receipt", |txn| {
                let fire = &fire;
                Box::pin(async move { stage_fire_receipt(txn, fire).await })
            })
            .await
            .unwrap();
        let retained = &case["retained"];
        let old_doc = if retained.is_object() {
            Some(goal_document(&access, json!({
                "goal_id":"task-goal",
                "node_did": if retained["owner"] == "owner-a" { identity.did() } else { other.did() },
                "session_id": retained["session"],
                "objective":"Original Task assignment",
                "status":retained["status"],
                "assignment_root_request_doc_id": if retained["assignment"]["source_doc_id"] == "retained" { &root } else { &next_root },
                "tokens_used":317, "token_usage_baseline":100, "active_time_seconds":23
            })).await)
        } else {
            None
        };
        let settled = case["settled"].as_bool().unwrap();
        if settled {
            let binding = GoalOutcomeBinding {
                owner_did: identity.did().into(),
                session_id: session.into(),
                assignment_request_id: fire.request_id.clone(),
                status: retained["status"].as_str().unwrap().into(),
            };
            access
                .transact_write("test.retained_outcome_settled", |txn| {
                    let fire = &fire;
                    let binding = &binding;
                    Box::pin(async move {
                        stage_outcome(
                            txn,
                            fire,
                            OutcomeStage {
                                goal: Some(binding),
                                assignment_replaced: false,
                                request_terminal: true,
                                terminal_state: "completed",
                                reason: &binding.status,
                            },
                            "2026-10-21T00:00:01Z",
                        )
                        .await
                    })
                })
                .await
                .unwrap();
        }
        let before = outcome_rows(&access).await;
        let current = &case["current"];
        if reused {
            access.write("test.reuse_task_goal_assignment", &format!(
                r#"mutation {{update_Goal(docID: "{}", input: {{assignment_root_request_doc_id: "{}", status: "active", objective: "New Task assignment"}}) {{_docID}}}}"#,
                escape_graphql_string(old_doc.as_ref().unwrap()), escape_graphql_string(&next_root)
            )).await.unwrap();
        } else {
            if let Some(doc) = &old_doc {
                access
                    .write(
                        "test.retire_task_goal",
                        &format!(
                            r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#,
                            escape_graphql_string(doc)
                        ),
                    )
                    .await
                    .unwrap();
            }
            if current.is_object() {
                goal_document(&access, json!({
                    "goal_id":"fresh-goal",
                    "node_did":if current["owner"] == "owner-a" { identity.did() } else { other.did() },
                    "session_id":current["session"], "objective":"Fresh assignment", "status":current["status"],
                    "assignment_root_request_doc_id":next_root,
                    "tokens_used":0, "token_usage_baseline":417, "active_time_seconds":0
                })).await;
            }
        }
        let signed = access
            .execute(&format!(
                "{{AgentRequest {{{}}}}}",
                crate::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let requests: Vec<gents_protocol::row::AgentRequestRow> =
            serde_json::from_value(signed["data"]["AgentRequest"].clone()).unwrap();
        for request in requests {
            crate::verify_request_receipt_signature(&request).unwrap();
        }
        if settled {
            access
                .write(
                    "test.retire_settled_request",
                    &format!(
                        r#"mutation {{delete_AgentRequest(docID: "{}") {{_docID}}}}"#,
                        escape_graphql_string(&root)
                    ),
                )
                .await
                .unwrap();
        }
        let rollback: Result<()> = access
            .transact_write("test.retained_outcome_rollback", |txn| {
                let owner = identity.did();
                Box::pin(async move {
                    recover_outcomes_in_txn(txn, owner).await?;
                    anyhow::bail!("injected outcome publication rollback")
                })
            })
            .await;
        assert!(
            rollback
                .unwrap_err()
                .to_string()
                .contains("injected outcome publication rollback"),
            "{name}"
        );
        assert_eq!(outcome_rows(&access).await, before, "{name}: rollback");
        drop(access);
        node.shutdown().await;
        drop(node);
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .data_path(&path)
                .build()
                .await
                .unwrap(),
        );
        let access = ConfigAccess::local(node.clone());
        let due = case["recover_reason"].as_str();
        assert_eq!(
            recover_outcomes(&node, identity.did()).await.unwrap(),
            usize::from(due.is_some()),
            "{name}: recovery"
        );
        assert_eq!(
            recover_outcomes(&node, identity.did()).await.unwrap(),
            0,
            "{name}: replay"
        );
        let after = outcome_rows(&access).await;
        assert_eq!(
            after.len(),
            before.len() + usize::from(due.is_some()),
            "{name}"
        );
        if let Some(reason) = due {
            assert_eq!(after[0]["terminal_state"], reason, "{name}");
            assert_eq!(after[0]["request_id"], fire.request_id, "{name}");
        } else {
            assert_eq!(after, before, "{name}: settled outcome changed");
        }
        node.shutdown().await;
    }
}
