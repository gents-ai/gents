use super::plan::PlannedWrite;
use super::*;
use crate::goal::operator_resume_test_support::Fixture;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::goal_control::SessionGoalObservation;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::{json, Value};

async fn foreign_source(
    root: &KeyIdentity,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    agent: &dyn NodeIdentity,
) -> (Arc<EmbeddedNode>, String) {
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
    let node_did = escape_graphql_string(root.did());
    access
        .write(
            "test.remote_source.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{node_did}"}}) {{_docID}} }}"#),
        )
        .await
        .unwrap();
    crate::test_support::install_test_agent_binding(&node, root.did(), "foreign caller", binding)
        .await;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "caller",
        root.did(),
        agent.did(),
        agent.did(),
        "caller-session",
        "Send to another chat",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(agent.did()),
    );
    create.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: "caller-goal".into(),
        root_request_doc_id: Some("caller-root".into()),
    });
    crate::sign_agent_request_create(agent, &mut create)
        .await
        .unwrap();
    let created = access
        .write(
            "test.remote_source.request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc_id = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    (node, doc_id)
}

#[tokio::test]
async fn generated_session_messages_capture_target_goal_before_signing() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["session_message_goal_input_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 11);
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let f = Fixture::new(&before).await;
        let temp = tempfile::tempdir().unwrap();
        let foreign_root =
            KeyIdentity::load_or_create(temp.path().join("foreign-root"), None).unwrap();
        let foreign_binding =
            crate::store::identity::provision_agent_identity(&foreign_root).unwrap();
        let foreign_agent =
            crate::store::identity::resolve_agent_identity(&foreign_root, &foreign_binding)
                .unwrap();
        let remote = case["owner"] == 2;
        let foreign_source = if remote {
            Some(foreign_source(&foreign_root, &foreign_binding, foreign_agent.as_ref()).await)
        } else {
            None
        };
        let caller_agent = if remote { &foreign_agent } else { &f.agent };
        let session_id = format!("goal-input-{index}");
        let target = SessionMessageTarget {
            node_did: f.identity.did().into(),
            agent_did: f.agent.did().into(),
            session_id: session_id.clone(),
        };
        let agent_access = f.agent_access();
        if case["session_readable"] == true {
            agent_access
                .transact_write("test.goal_input.session", |txn| {
                    Box::pin(async {
                        crate::session::ensure_session_in_txn(
                            txn,
                            &session_id,
                            f.identity.did(),
                            f.agent.did(),
                            Some(caller_agent.did()),
                            None,
                            None,
                            "2026-01-01T00:00:00Z",
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            let session_doc = agent_access
                .transact_readonly("test.goal_input.session_doc", |txn| {
                    Box::pin(async {
                        Ok::<_, anyhow::Error>(
                            crate::session::load_agent_session_owner_in_txn(
                                txn,
                                f.identity.did(),
                                &session_id,
                            )
                            .await?
                            .unwrap()
                            .doc_id,
                        )
                    })
                })
                .await
                .unwrap();
            agent_access
                .add_resource_relationship(
                    "AgentSession",
                    &session_doc,
                    "parent",
                    "agents",
                    &f.agent_doc_id,
                )
                .await
                .unwrap();
            if remote {
                agent_access
                    .add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "reader",
                        &identity::Did::new(caller_agent.did()).unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
        let mut goal_id = None;
        let mut root_doc = None;
        if case["observed"].is_object() && case["session_readable"] == false {
            agent_access.write("test.goal_input.orphan", &format!(r#"mutation {{ create_Goal(input: {{ goal_id: "orphan", node_did: "{}", agent_did: "{}", session_id: "{}", objective: "Orphan", status: "active" }}) {{ _docID }} }}"#,
                escape_graphql_string(f.identity.did()), escape_graphql_string(f.agent.did()), escape_graphql_string(&session_id))).await.unwrap();
        }
        if case["observed"].is_object() && case["session_readable"] == true {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("opening-{index}"),
                f.identity.did(),
                f.agent.did(),
                f.agent.did(),
                &session_id,
                "Original goal",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(f.agent.did()),
            );
            crate::sign_agent_request_create(f.agent.as_ref(), &mut opening)
                .await
                .unwrap();
            let created = agent_access
                .write(
                    "test.goal_input.opening",
                    &opening.graphql_mutation().unwrap(),
                )
                .await
                .unwrap();
            let doc = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
            let goal = crate::goal::set_goal_from_access(
                &agent_access,
                f.identity.did(),
                &session_id,
                Some("Target goal"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
            goal_id = Some(goal.goal_id);
            if !case["observed"]["root"].is_null() {
                agent_access.write("test.goal_input.root", &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ assignment_root_request_doc_id: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&goal.doc_id), escape_graphql_string(&doc))).await.unwrap();
                root_doc = Some(doc);
            }
        }
        if remote && (case["observation_verified"] == true || case["observed"].is_object()) {
            let runtime: Arc<dyn NodeIdentity> = f.identity.clone();
            crate::goal::project_session_goal(
                &f.node,
                &runtime,
                &f.agent,
                &session_id,
                Some(caller_agent.did()),
            )
            .await
            .unwrap();
            let rows = agent_access.execute(&format!(r#"{{ GoalObservation(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ _docID observation_json }} }}"#,
                escape_graphql_string(f.identity.did()), escape_graphql_string(&session_id))).await.unwrap();
            let row = &rows["data"]["GoalObservation"][0];
            let mut observation: SessionGoalObservation =
                serde_json::from_str(row["observation_json"].as_str().unwrap()).unwrap();
            if let Some(goal) = &mut observation.goal {
                if case["observed"]["owner"] != case["owner"] {
                    goal.snapshot.node_did = foreign_root.did().into();
                }
                if case["observed"]["session"] != case["session"] {
                    goal.snapshot.session_id = "foreign-chat".into();
                }
            }
            observation.signature = f
                .agent
                .sign(&observation.signing_payload().unwrap())
                .await
                .unwrap();
            if case["observation_verified"] == false {
                observation.signature[0] ^= 1;
            }
            agent_access.write("test.goal_input.observation", &format!(r#"mutation {{ update_GoalObservation(docID: "{}", input: {{ observation_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(row["_docID"].as_str().unwrap()),
                escape_graphql_string(&serde_json::to_string(&observation).unwrap()))).await.unwrap();
        }
        let cause = SessionMessageCause {
            caller_node_did: if remote {
                foreign_root.did().into()
            } else {
                f.identity.did().into()
            },
            caller_agent_did: caller_agent.did().into(),
            caller_session_id: if remote {
                "caller-session".into()
            } else {
                f.parent.session_id.clone()
            },
            caller_requester_did: if remote {
                Some(caller_agent.did().into())
            } else {
                f.parent.requester_did.clone()
            },
            caller_request_id: if remote {
                "caller".into()
            } else {
                f.parent.request_id.clone()
            },
            caller_request_doc_id: foreign_source
                .as_ref()
                .map_or_else(|| f.parent.doc_id.clone(), |(_, doc)| doc.clone()),
            caller_hop: 0,
            tool_call_id: format!("call-{index}"),
            tool_call_doc_id: format!("call-doc-{index}"),
            correlation: None,
        };
        let selected = super::plan::target_goal_assignment(&f.node, &cause, &target).await;
        if case["expected"]["kind"] == "pending" {
            assert!(
                selected.is_err(),
                "{name}: unknown goal must remain pending"
            );
            let source_node = foreign_source.as_ref().map_or(&f.node, |(node, _)| node);
            let signer = caller_agent.managed_agent_signer().unwrap();
            let planned = plan(
                source_node,
                &cause,
                &target,
                RenderedBody {
                    content: "Follow up".into(),
                    goal: None,
                },
                None,
                false,
                &signer,
            )
            .await;
            assert!(
                planned.as_ref().map_or(true, |result| result.is_err()),
                "{name}: unknown destination input must not be signed as unassigned"
            );
        } else {
            let expected = case["expected"]["reference"]
                .is_object()
                .then(|| GoalAssignmentInput {
                    goal_id: goal_id.unwrap(),
                    root_request_doc_id: root_doc,
                });
            assert_eq!(selected.unwrap(), expected, "{name}");
            if remote {
                // Cross-runtime delivery is refused until the authenticated HTTP path exists,
                // so a remote caller's goal input is never planned or signed here.
                let signer = caller_agent.managed_agent_signer().unwrap();
                let source_node = foreign_source.as_ref().map_or(&f.node, |(node, _)| node);
                let refused = plan(
                    source_node,
                    &cause,
                    &target,
                    RenderedBody {
                        content: "Follow up".into(),
                        goal: None,
                    },
                    None,
                    false,
                    &signer,
                )
                .await
                .unwrap();
                assert!(
                    refused.is_err_and(|error| error.contains("cross-runtime agent delivery")),
                    "{name}: remote delivery must be refused"
                );
            } else {
                let signer = caller_agent.managed_agent_signer().unwrap();
                let source_node = foreign_source.as_ref().map_or(&f.node, |(node, _)| node);
                let source_access = ConfigAccess::local_managed_agent(
                    source_node.clone(),
                    identity::Did::new(caller_agent.did()).unwrap(),
                    caller_agent.as_ref(),
                )
                .unwrap();
                let planned = plan(
                    source_node,
                    &cause,
                    &target,
                    RenderedBody {
                        content: "Follow up".into(),
                        goal: None,
                    },
                    None,
                    false,
                    &signer,
                )
                .await
                .unwrap()
                .unwrap_or_else(|error| panic!("{name}: modeled delivery requires authenticated cross-runtime transport: {error}"));
                let PlannedWrite::Request { create, .. } = &planned.write else {
                    panic!("{name}: expected a session message request")
                };
                assert_eq!(
                    create.input.goal_assignment, expected,
                    "{name}: signed capture"
                );
                assert!(
                    caller_agent
                        .verify(
                            caller_agent.did(),
                            &create.signing_payload(),
                            &create.admission.signature
                        )
                        .await
                        .unwrap(),
                    "{name}: input must be signed by the selected Agent"
                );
                assert_ne!(
                    create
                        .input
                        .goal_assignment
                        .as_ref()
                        .map(|goal| goal.goal_id.as_str()),
                    Some("caller-goal"),
                    "{name}"
                );
                if name == "local_target_uses_own_goal" {
                    agent_access.write("test.goal_input.later_root", &format!(
                        r#"mutation {{ update_Goal(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, input: {{ assignment_root_request_doc_id: "later-opening" }}) {{ _docID }} }}"#,
                        escape_graphql_string(&target.node_did), escape_graphql_string(&target.session_id),
                    )).await.unwrap();
                }
                let staged = source_access
                    .transact_write("test.session_message_stage_goal_input", |txn| {
                        Box::pin(async { stage_planned_write_in_txn(txn, &cause, &planned).await })
                    })
                    .await
                    .unwrap();
                let stored = source_access
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                        escape_graphql_string(&staged.doc_id),
                        crate::request::admission::SIGNED_REQUEST_FIELDS,
                    ))
                    .await
                    .unwrap();
                let stored: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(stored["data"]["AgentRequest"][0].clone()).unwrap();
                crate::request::admission::verify_request_receipt_signature(&stored).unwrap();
                assert_eq!(
                    stored
                        .input
                        .as_ref()
                        .and_then(|input| input.goal_assignment.clone()),
                    expected,
                    "{name}: persisted capture"
                );
                if let Some(expected) = &expected {
                    let crate::goal::RequestAssignmentObservation::Assigned(assigned) =
                        crate::goal::load_request_assignment(&source_access, &stored)
                            .await
                            .unwrap()
                    else {
                        panic!("{name}: persisted signed message lost its retained assignment")
                    };
                    assert_eq!(assigned.goal_id, expected.goal_id, "{name}");
                    assert_eq!(
                        assigned.assignment_root_request_doc_id, expected.root_request_doc_id,
                        "{name}"
                    );
                }
            }
        }
        f.node.shutdown().await.expect("shut down test node");
    }
}
