use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use std::sync::Arc;

#[test]
fn generated_continuation_assignments_preserve_physical_bindings() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().goal_continuation_assignment_cases;
    assert_eq!(cases.len(), 17);
    for case in cases {
        let parent: Option<GoalAssignmentInput> =
            serde_json::from_value(case["parent"].clone()).unwrap();
        let expected: Option<GoalAssignmentInput> =
            serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(
            continuation_reference(
                case["goal"].as_str().unwrap(),
                case["current"].as_str(),
                case["parent_doc"].as_str().unwrap(),
                parent.as_ref(),
                case["receipt_doc"].as_str(),
            ),
            expected,
            "{}",
            case["name"]
        );
        for edge in case["edges"].as_array().unwrap() {
            let child: Option<GoalAssignmentInput> =
                serde_json::from_value(edge["child"].clone()).unwrap();
            assert_eq!(
                continuation_reference_matches(
                    case["goal"].as_str().unwrap(),
                    case["parent_doc"].as_str().unwrap(),
                    parent.as_ref(),
                    child.as_ref(),
                ),
                edge["accepted"].as_bool().unwrap(),
                "{}: {edge}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn generated_first_request_bindings_commit_exact_physical_roots() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().goal_first_request_binding_cases;
    assert_eq!(cases.len(), 14);
    for case in cases {
        let temp = tempfile::tempdir().unwrap();
        let signer = KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(signer.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            signer.did(),
            signer.did(),
            "agent",
            "session",
            "Start work",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(signer.did()),
        );
        crate::sign_agent_request_create(&signer, &mut create)
            .await
            .unwrap();
        let response = access
            .write(
                "test.first_binding.request",
                &create.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let physical = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        super::super::create_goal_for_session(&node, signer.did(), "session", "Start work", None)
            .await
            .unwrap();
        let mut goal = load_canonical_goal(&node, signer.did(), "session")
            .await
            .unwrap()
            .unwrap();
        let root = |value: &str| {
            if value == "request" {
                physical.clone()
            } else {
                value.to_owned()
            }
        };
        if let Some(current) = case["current"].as_str() {
            access.write("test.first_binding.previous_root", &format!(
                "mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
                escape_graphql_string(&goal.doc_id), escape_graphql_string(&root(current)),
            )).await.unwrap();
            goal = load_canonical_goal(&node, signer.did(), "session")
                .await
                .unwrap()
                .unwrap();
        }
        let mut reference: Option<GoalAssignmentInput> =
            serde_json::from_value(case["reference"].clone()).unwrap();
        if let Some(reference) = reference.as_mut() {
            if reference.goal_id == "goal" {
                reference.goal_id = goal.goal_id.clone();
            }
        }
        if case["goal"] == "" {
            goal.goal_id.clear();
        }
        let request_doc = root(case["request_doc"].as_str().unwrap());
        let result = access
            .transact_write("test.first_binding.transaction", |txn| {
                let goal = &goal;
                let request_doc = &request_doc;
                let reference = reference.as_ref();
                Box::pin(async move {
                    bind_goal_first_request_in_txn(txn, goal, request_doc, reference).await?;
                    anyhow::ensure!(case["commit"] == true, "injected publication discard");
                    Ok(())
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            case["accepted"] == true && case["commit"] == true,
            "{}: {result:?}",
            case["name"]
        );
        let after = load_canonical_goal(&node, signer.did(), "session")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.assignment_root_request_doc_id,
            case["expected_root"].as_str().map(root),
            "{}",
            case["name"]
        );

        let task_session = "task-session";
        let initial = deterministic_goal_id(signer.did(), task_session);
        let task_id = |value: &str| {
            if value == "initial-goal" {
                initial.clone()
            } else {
                value.to_owned()
            }
        };
        let selected = task_assignment_goal_id(
            case["current_task_goal"].as_str().map(&task_id).as_deref(),
            signer.did(),
            task_session,
        );
        assert_eq!(
            selected,
            task_id(case["expected_task_goal"].as_str().unwrap())
        );
        if let Some(observed) = case["observed_task_goal"].as_str() {
            let identity = GoalCreationIdentity {
                goal_id: task_id(observed),
                creation_key: format!("task-create:{}", case["name"]),
            };
            access
                .transact_write("test.first_binding.task_goal", |txn| {
                    let identity = &identity;
                    let signer = &signer;
                    Box::pin(async move {
                        super::super::stage_goal_and_claim(
                            txn,
                            signer.did(),
                            task_session,
                            "Task work",
                            None,
                            GoalState {
                                status: GoalStatus::Active,
                                blocked_audits: 0,
                                wrapup_requested: false,
                                wrapup_completed: false,
                            },
                            "2026-10-21T00:00:00Z",
                            Some(identity),
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
        }
        let task_reference = GoalAssignmentInput {
            goal_id: selected.clone(),
            root_request_doc_id: None,
        };
        let checked = access
            .transact_readonly("test.first_binding.task_identity", |txn| {
                let signer = &signer;
                let selected = &selected;
                let task_reference = &task_reference;
                Box::pin(async move {
                    validate_task_goal_identity_in_txn(
                        txn,
                        signer.did(),
                        task_session,
                        selected,
                        Some(task_reference),
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            checked.is_ok(),
            case["task_binding_valid"] == true,
            "{}: {checked:?}",
            case["name"]
        );
        crate::test_support::install_test_agent(&node, signer.did(), "agent").await;
        access
            .transact_write("test.first_binding.task_session", |txn| {
                let signer = &signer;
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        task_session,
                        signer.did(),
                        "agent",
                        Some(signer.did()),
                        None,
                        None,
                        "2026-10-21T00:00:00Z",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let fire_identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: signer.did().to_owned(),
            trigger_id: "manual:task".into(),
            source_collection: "Task".into(),
            source_doc_id: "task-binding".into(),
        };
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            fire_key: fire_identity.fire_key(),
            request_id: fire_identity.request_id(),
            identity: fire_identity,
            task_id: "task".into(),
            session_id: task_session.into(),
            goal_id: Some(selected.clone()),
            goal_objective: Some("Task work".into()),
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-10-21T00:00:00Z".into(),
        };
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &fire.request_id,
            signer.did(),
            signer.did(),
            "agent",
            task_session,
            "Task work",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(signer.did()),
        );
        request.caused_by_trigger_kind = Some("manual".into());
        request.input.goal_assignment = Some(task_reference);
        crate::sign_agent_request_create(&signer, &mut request)
            .await
            .unwrap();
        let admitted = crate::lifecycle::write_task_delivery(&access, &fire, true, &request).await;
        assert_eq!(
            admitted.is_ok(),
            case["task_binding_valid"] == true,
            "{}: admission error {:?}",
            case["name"],
            admitted.as_ref().err()
        );
        if let Ok(admitted) = admitted {
            let persisted = crate::request_binding::load_agent_request_by_doc_id(
                &node,
                &admitted.request.doc_id,
            )
            .await
            .unwrap()
            .unwrap();
            access
                .transact_write("test.first_binding.task_claim", |txn| {
                    let persisted = &persisted;
                    Box::pin(async move {
                        super::super::apply_claimed_task_goal_in_txn(
                            txn,
                            persisted,
                            "2026-10-21T00:00:01Z",
                        )
                        .await
                    })
                })
                .await
                .unwrap();
            let assigned = load_canonical_goal(&node, signer.did(), task_session)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(assigned.goal_id, selected);
            assert_eq!(
                assigned.assignment_root_request_doc_id.as_deref(),
                Some(admitted.request.doc_id.as_str())
            );
            assert_eq!(
                persisted.input.goal_assignment,
                request.input.goal_assignment
            );
        } else {
            let state = access
                .execute("{TriggerFire {fire_key} AgentRequest {session_id}}")
                .await
                .unwrap();
            assert!(state["data"]["TriggerFire"].as_array().unwrap().is_empty());
            assert!(!state["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["session_id"] == task_session));
        }
        node.shutdown().await;
    }
}
