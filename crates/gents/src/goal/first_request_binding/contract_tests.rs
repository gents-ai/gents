use super::*;
use crate::store::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use std::sync::Arc;

async fn fixture_session(
    access: &ConfigAccess,
    node_did: &str,
    user_did: &str,
    agent_did: &str,
    agent_doc: &str,
    session_id: &str,
) -> String {
    access.transact_write("test.first_binding.session", |txn| {
        Box::pin(async move {
            let input = serde_json::json!({"node_did": node_did, "session_id": session_id,
                "requester_did": user_did, "agent_did": agent_did,
                "relationships_pending": false, "created_at": "2026-11-04T00:00:00Z"});
            let created = txn.execute_client_signed_create(
                "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                &serde_json::json!({"input": input}), gents_protocol::schemas::AGENT_SESSION,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&created, "AgentSession")?;
            txn.add_resource_relationship("AgentSession", &doc, "parent", "agents", agent_doc).await?;
            txn.add_actor_relationship("AgentSession", &doc, "working_agent", &identity::Did::new(agent_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap()
}

async fn fixture_goal(access: &ConfigAccess, node_did: &str, session_id: &str) -> GoalDocument {
    access
        .transact_readonly("test.first_binding.goal", |txn| {
            Box::pin(load_canonical_goal_in_txn(txn, node_did, session_id))
        })
        .await
        .unwrap()
        .unwrap()
}

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
        let user = KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&signer).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&signer, &binding).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(signer.did())
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, user.did())
                .with_client_signing_capability(user.client_signing_capability().unwrap())
                .unwrap()
                .with_client_signed_agent_creates(),
        );
        let agent_doc = access.transact_write("test.first_binding.agent", |txn| {
            Box::pin(async {
                let created = txn.execute_client_signed_create(
                    "mutation($input: AgentMutationInputArg!) {create_Agent(input: $input) {_docID}}",
                    &serde_json::json!({"input": {"node_did": signer.did(), "agent_did": agent.did(),
                        "creation_nonce": binding.creation_nonce, "display_name": "test agent", "inference_profile_id": "test-profile"}}),
                    gents_protocol::schemas::AGENT,
                ).await?;
                let doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
                txn.add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(agent.did())?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        }).await.unwrap();
        let session_doc = fixture_session(
            &access,
            signer.did(),
            user.did(),
            agent.did(),
            &agent_doc,
            "session",
        )
        .await;
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            signer.did(),
            user.did(),
            agent.did(),
            "session",
            "Start work",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        create.session_doc_id = Some(session_doc.clone());
        create.agent_doc_id = Some(agent_doc.clone());
        crate::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        crate::request::lifecycle::submit_client_request(&access, &create)
            .await
            .unwrap();
        let request = access
            .execute(r#"{AgentRequest(filter: {request_id: {_eq: "first"}}) {_docID}}"#)
            .await
            .unwrap();
        let physical = request["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        super::super::create_goal_for_session_from_access(
            &access,
            signer.did(),
            "session",
            "Start work",
            None,
        )
        .await
        .unwrap();
        let mut goal = fixture_goal(&access, signer.did(), "session").await;
        let root = |value: &str| {
            if value == "request" {
                physical.clone()
            } else {
                value.to_owned()
            }
        };
        if let Some(current) = case["current"].as_str() {
            let doc = escape_graphql_string(&goal.doc_id);
            let current = root(current);
            access.transact_write("test.first_binding.previous_root", |txn| {
                let current = &current;
                let doc = &doc;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        &format!(r#"mutation($input: GoalMutationInputArg!) {{update_Goal(docID: "{doc}", input: $input) {{_docID}}}}"#),
                        &serde_json::json!({"input": {"assignment_root_request_doc_id": current}}),
                        gents_protocol::schemas::GOAL,
                    ).await
                })
            }).await.unwrap();
            goal = fixture_goal(&access, signer.did(), "session").await;
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
        let after = fixture_goal(&access, signer.did(), "session").await;
        assert_eq!(
            after.assignment_root_request_doc_id,
            case["expected_root"].as_str().map(root),
            "{}",
            case["name"]
        );

        let task_session = "task-session";
        fixture_session(
            &access,
            signer.did(),
            user.did(),
            agent.did(),
            &agent_doc,
            task_session,
        )
        .await;
        let initial = deterministic_goal_id(signer.did(), task_session);
        let task_id = |value: &str| {
            if value == "initial-goal" {
                initial.clone()
            } else {
                value.to_owned()
            }
        };
        let selected = task_assignment_goal_id(
            case["current_task_goal"].as_str().map(task_id).as_deref(),
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
                            None,
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
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
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
            user.did(),
            agent.did(),
            task_session,
            "Task work",
            "scheduled",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(agent.did(), "manual:task"),
        );
        request.caused_by_trigger_id = Some("manual:task".into());
        request.caused_by_trigger_kind = Some("schedule".into());
        request.input.goal_assignment = Some(task_reference);
        crate::sign_agent_request_create(agent.as_ref(), &mut request)
            .await
            .unwrap();
        let admitted =
            crate::request::lifecycle::write_task_delivery(&agent_access, &fire, true, &request)
                .await;
        assert_eq!(
            admitted.is_ok(),
            case["task_binding_valid"] == true,
            "{}: admission error {:?}",
            case["name"],
            admitted.as_ref().err()
        );
        if let Ok(admitted) = admitted {
            let persisted = crate::request::binding::load_agent_request_by_doc_id_with_access(
                &agent_access,
                &admitted.request.doc_id,
            )
            .await
            .unwrap()
            .unwrap();
            agent_access
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
            let assigned = fixture_goal(&agent_access, signer.did(), task_session).await;
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
            let state = agent_access
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
        node.shutdown().await.expect("node shutdown");
    }
}
